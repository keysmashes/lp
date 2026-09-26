mod animations;
mod ui;

use std::{
    collections::{HashMap, hash_map::DefaultHasher},
    hash::{Hash, Hasher},
    io::{BufRead, BufReader},
    process,
    sync::{atomic::AtomicBool, mpsc, Arc},
    thread,
    time::Duration,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eyre::WrapErr;
use i3_ipc::{Connect, I3};
use itertools::Itertools;
use parking_lot::Mutex;
use tap::Pipe;

use lp::{
    coords_to_key, key_to_coords, rect, Color, Command, ComplexColor, Key, Launchpad, Message,
    SimpleColor,
};

fn configure_signals(tx: mpsc::Sender<Event>) -> eyre::Result<()> {
    let interrupted = Arc::new(AtomicBool::new(false));
    for signal in signal_hook::consts::TERM_SIGNALS {
        // if we can't shut down cleanly, just exit...
        signal_hook::flag::register_conditional_shutdown(*signal, 1, Arc::clone(&interrupted))
            .wrap_err("couldn't register fallback shutdown hook")?;
        // ...but try to shut down cleanly first.
        signal_hook::flag::register(*signal, Arc::clone(&interrupted))
            .wrap_err("couldn't register shutdown hook")?;
    }
    let mut signals = signal_hook::iterator::Signals::new(signal_hook::consts::TERM_SIGNALS)
        .wrap_err("couldn't register interest in shutdown signals")?;
    thread::Builder::new()
        .name("lp signal handler".into())
        .spawn(move || {
            signals.forever().next();
            tx.send(Event::Exit).unwrap();
        })
        .wrap_err("couldn't spawn signal handler thread")?;
    Ok(())
}

fn _stress_test(launchpad: &mut Launchpad) -> eyre::Result<()> {
    let mut vec_a = vec![];
    let mut vec_b = vec![];
    let mut vec_c = vec![];
    for i in 11..=19 {
        for j in 0..=8 {
            vec_a.push((i + 10 * j, ComplexColor::Rgb(127, 0, 0)));
            vec_b.push((i + 10 * j, ComplexColor::Rgb(0, 127, 0)));
            vec_c.push((i + 10 * j, ComplexColor::Rgb(0, 0, 127)));
        }
    }
    let a = Command::SetColors(&vec_a);
    let b = Command::SetColors(&vec_b);
    let c = Command::SetColors(&vec_c);
    const US: u64 = 4000;
    for _ in 0..100 {
        launchpad.send(&a)?;
        thread::sleep(Duration::from_micros(US));
        launchpad.send(&b)?;
        thread::sleep(Duration::from_micros(US));
        launchpad.send(&c)?;
        thread::sleep(Duration::from_micros(US));
    }
    launchpad.send(&Command::KeyOn(55, SimpleColor::Static(13)))?;
    Ok(())
}

#[derive(Debug, Eq, PartialEq)]
#[non_exhaustive]
enum Event {
    KeyDown(Key),
    KeyUp(Key),
    Brightness(u8),
    I3,
    MediaPlaying(bool),
    Redraw,
    Exit,
}

#[allow(static_mut_refs)] // TODO: don't
fn main() -> eyre::Result<()> {
    let (tx, rx) = mpsc::channel();

    configure_signals(tx.clone()).wrap_err("couldn't set up signal handlers")?;

    thread::spawn({
        let tx = tx.clone();
        move || {
            use i3_ipc::{
                event::{
                    Event,
                    Subscribe::{Output, Workspace},
                },
                I3Stream,
            };
            let mut i3 = I3Stream::conn_sub([Workspace, Output]).unwrap();
            for event in i3.iter() {
                match event.unwrap() {
                    Event::Workspace(_) => tx.send(crate::Event::I3).unwrap(),
                    Event::Output(_) => tx.send(crate::Event::I3).unwrap(),
                    Event::Mode(_) => unreachable!(),
                    Event::Window(_) => unreachable!(),
                    Event::BarConfig(_) => unreachable!(),
                    Event::Binding(_) => unreachable!(),
                    Event::Shutdown(_) => todo!(),
                    Event::Tick(_) => {}
                }
            }
        }
    });

    thread::spawn({
        let tx = tx.clone();
        move || {
            for line in BufReader::new(
                duct::cmd!("playerctl", "-F", "status")
                    .unchecked()
                    .reader()
                    .unwrap(),
            )
            .lines()
            {
                match line.unwrap().as_str() {
                    "Playing" => tx.send(Event::MediaPlaying(true)).unwrap(),
                    "Paused" | "Stopped" => tx.send(Event::MediaPlaying(false)).unwrap(),
                    "" => {}
                    x => unreachable!("playerctl output: {:?}", x),
                }
            }
        }
    });

    thread::spawn({
        let tx = tx.clone();
        move || {
            const T: Duration = Duration::from_secs(10);
            loop {
                thread::sleep(T);
                tx.send(Event::Redraw).unwrap();
            }
        }
    });

    let mut launchpad = {
        let tx = tx.clone();
        Launchpad::connect(move |_ts, message| match message {
            Message::KeyDown(key) => tx.send(Event::KeyDown(key)).unwrap(),
            Message::KeyUp(key) => tx.send(Event::KeyUp(key)).unwrap(),
            Message::ProgrammerMode(_) => {}
            Message::Brightness(brightness) => tx.send(Event::Brightness(brightness)).unwrap(),
            message => unimplemented!("{:?}", message),
        })
        .wrap_err("couldn't connect to Launchpad")?
    };

    launchpad.send(&Command::SetAwake(true))?;
    animations::startup(&mut launchpad).wrap_err("couldn't display startup animation")?;
    // discard events that arrived during the startup animation
    for _ in rx.try_iter() {}
    tx.send(Event::Redraw)?;

    let mut keypad = HashMap::with_capacity(81);
    for key in rect(11, 88) {
        keypad.insert(key, false);
    }

    let mut fb = HashMap::with_capacity(81);
    for key in rect(11, 99) {
        fb.insert(key, Color::Simple(SimpleColor::Static(0)));
    }

    let host = cpal::default_host();
    let device = host.default_output_device().unwrap();
    let config = device.default_output_config().unwrap();
    struct NoteState {
        input: bool,
        volume: f32,
        clock: f32,
        freq: f32,
    }
    impl NoteState {
        fn new(freq: f32) -> Self {
            NoteState {
                input: false,
                volume: 0.0,
                clock: 0.0,
                freq,
            }
        }
    }
    struct AudioState {
        notes: HashMap<usize, NoteState>,
        config: Option<cpal::StreamConfig>,
        glitch_mode: bool,
        no_tanh: bool,
        alpha: f32,
        forget_copysign: bool,
    }
    fn get_audio_frame(audio_state: &mut AudioState, len: usize) -> f32 {
        let mut value: f32 = 0.0;
        let sample_rate = audio_state.config.as_ref().unwrap().sample_rate as f32;
        for (_, state) in audio_state.notes.iter_mut() {
            if state.input && state.volume < 1.0 {
                state.volume = (state.volume + 0.02).clamp(0.0, 1.0);
            }
            if state.volume > 0.0 {
                let phi = if audio_state.glitch_mode {
                    // glitch album time
                    state.clock += state.freq / sample_rate;
                    state.clock
                } else {
                    state.clock += len as f32;
                    // figure out some multiple of the period (breaks if bottom-heavy, so pick a high multiplier)
                    let period = sample_rate / state.freq;
                    // prevent the clock running away
                    while state.clock > period {
                        state.clock -= period;
                    }
                    state.clock / sample_rate
                };
                let sample = (state.freq * std::f32::consts::TAU * phi).sin();
                value += sample * 0.2 * state.volume;
                if !state.input {
                    state.volume -= 0.0004;
                }
            } else {
                state.clock = 0.0;
            }
        }
        if audio_state.no_tanh {
            value
        } else {
            value
                .powi(2)
                .powf(audio_state.alpha / 2.0)
                .tanh()
                .powf(1.0 / audio_state.alpha)
                .pipe(|x| {
                    if audio_state.forget_copysign {
                        x
                    } else {
                        x.copysign(value)
                    }
                })
        }
    }
    let audio_state = Arc::new(Mutex::new(AudioState {
        notes: HashMap::new(),
        config: None,
        glitch_mode: false,
        no_tanh: false,
        alpha: 3.0,
        forget_copysign: false,
    }));
    let stream = device.build_output_stream(
        &cpal::StreamConfig {
            channels: 1,
            sample_rate: 44100,
            buffer_size: cpal::BufferSize::Default,
        },
        {
            // https://github.com/0xC45/simple-synth/blob/42611692157830df9c17de10dd20abb4ee2806e1/src/main.rs#L236
            let state = audio_state.clone();
            let channels = config.channels() as usize;
            state.lock().config = Some(config.config());
            move |data: &mut [f32], _info| {
                for frame in data.chunks_mut(channels) {
                    let len = frame.len();
                    let v = get_audio_frame(&mut state.lock(), len);
                    for sample in frame.iter_mut() {
                        *sample = v;
                    }
                }
            }
        },
        |error| eprintln!("cpal error: {}", error),
        None,
    )?;
    stream.play()?;

    let mut i3 = I3::connect()?;
    let mut workspaces = i3.get_workspaces()?;
    let mut outputs = i3.get_outputs()?;
    let mut w_per_o = HashMap::new();
    let mut w_by_num = HashMap::new();
    for workspace in workspaces {
        // TODO: yuuuuck
        w_per_o
            .entry(workspace.output.clone())
            .or_insert(Vec::new())
            .push(workspace.num);
        w_by_num.entry(workspace.num).or_insert(workspace);
    }
    const I3_COLORS: &[u8] = &[21, 29, 37, 45];
    let output_colors: HashMap<&str, u8> = [
        ("DP-1", 29u8),
        ("DP-2", 21u8),
        ("HDMI-1", 37u8),
        ("HDMI-2", 45u8),
    ]
    .into_iter()
    .collect();

    for event in rx.iter() {
        if let Event::Exit = event {
            break;
        }
        if let Event::I3 = event {
            // TODO: this i3 stuff is *awful*
            workspaces = i3.get_workspaces()?;
            outputs = i3.get_outputs()?;
            w_per_o.clear();
            w_by_num.clear();
            for workspace in workspaces {
                w_per_o
                    .entry(workspace.output.clone())
                    .or_insert(Vec::new())
                    .push(workspace.num);
                w_by_num.entry(workspace.num).or_insert(workspace);
            }
        }
        // "overdraw is bad"? nah that doesn't sound right
        for key in rect(11, 99) {
            *fb.get_mut(&key).unwrap() = Color::Simple(SimpleColor::Static(0));
        }
        let mut ui = ui::Ui {
            fb: &mut fb,
            event,
            launchpad_for_side_effects: &mut launchpad,
            tx_for_side_effects: &tx,
        };
        if ui.cat_protection(29, Color::simple(55), &[11, 18, 81, 88]) && ui.awake(19, Color::Simple(SimpleColor::Static(47))) {
            let tab = ui.tabs::<4>(95);
            match tab {
                0 => {
                    // i3
                    // shift button
                    let i3_shift = ui.holdable_button(53, Color::simple(2), Color::simple(3));

                    // move
                    if ui.impulse_button(91, Color::simple(1), Color::simple(2)) {
                        i3.run_command(if i3_shift { "move up" } else { "focus up" })?;
                    }
                    if ui.impulse_button(92, Color::simple(1), Color::simple(2)) {
                        i3.run_command(if i3_shift { "move down" } else { "focus down" })?;
                    }
                    if ui.impulse_button(93, Color::simple(1), Color::simple(2)) {
                        i3.run_command(if i3_shift { "move left" } else { "focus left" })?;
                    }
                    if ui.impulse_button(94, Color::simple(1), Color::simple(2)) {
                        i3.run_command(if i3_shift {
                            "move right"
                        } else {
                            "focus right"
                        })?;
                    }
                    // workspaces
                    for workspace_num in 0..15 {
                        let color = {
                            if let Some(w) = w_by_num.get(&(workspace_num as i32)) {
                                let first_time = ui.monostable(w.urgent, workspace_num);
                                if w.urgent {
                                    // Color::simple(9)
                                    if first_time {
                                        animations::alert(
                                            ui.launchpad_for_side_effects,
                                            Some(
                                                81 - (workspace_num / 5 * 10) + (workspace_num % 5),
                                            ),
                                        )
                                        .wrap_err("couldn't display alert animation")?;
                                        ui.tx_for_side_effects.send(Event::Redraw).unwrap();
                                    }
                                    Color::Simple(SimpleColor::Pulsing(9))
                                } else {
                                    let mut hasher = DefaultHasher::new();
                                    w.output.hash(&mut hasher);
                                    let mut color = if let Some(c) = output_colors.get(&*w.output) {
                                        *c
                                    } else {
                                        I3_COLORS[hasher.finish() as usize % I3_COLORS.len()]
                                    };
                                    if !w.visible {
                                        color += 2;
                                    } else if w.focused {
                                        color -= 1;
                                    }
                                    Color::simple(color)
                                }
                            } else {
                                Color::simple(0)
                            }
                        };
                        // TODO: yuck (specifically, the `as`)
                        if ui.impulse_button(
                            81 - (workspace_num / 5 * 10) + (workspace_num % 5),
                            color,
                            color,
                        ) {
                            match w_by_num.get(&(workspace_num as i32)) {
                                Some(w) if w.focused && w.urgent => {
                                    i3.run_command("[urgent=latest workspace=__focused__] focus")?;
                                }
                                _ => {
                                    i3.run_command(format!(
                                        "{}workspace number {}",
                                        if i3_shift {
                                            format!(
                                                "move container to workspace number {}; ",
                                                workspace_num
                                            )
                                        } else {
                                            "".to_owned()
                                        },
                                        workspace_num
                                    ))?;
                                }
                            }
                        }
                    }
                    // outputs
                    let mut base = 81;
                    for output in outputs
                        .iter()
                        .filter(|o| o.active)
                        .sorted_by(|a, b| a.rect.x.cmp(&b.rect.x))
                        .sorted_by(|a, b| a.rect.y.cmp(&b.rect.y))
                    {
                        assert!(base >= 21);
                        // output
                        // TODO: if there is already another output button held down, do something
                        static mut CURRENT_OUTPUT_HELD: Option<String> = None;
                        let c = Color::simple(
                            if let Some(w) = &output
                                .current_workspace
                                .as_ref()
                                .unwrap()
                                .parse::<i32>()
                                .ok()
                                .and_then(|n| w_by_num.get(&n))
                            {
                                let mut hasher = DefaultHasher::new();
                                w.output.hash(&mut hasher);
                                let mut color = if let Some(c) = output_colors.get(&*w.output) {
                                    *c
                                } else {
                                    I3_COLORS[hasher.finish() as usize % I3_COLORS.len()]
                                };
                                if !w.focused {
                                    color += 2;
                                }
                                color
                            } else {
                                6 // should never happen?
                            },
                        );
                        if ui.impulse_button(base + 8, c, c) {
                            // Safety: still not
                            let new_output = &output.name;
                            let mut preaction = "".to_owned();
                            if let Some(old_output) = unsafe { &CURRENT_OUTPUT_HELD } {
                                // find the workspaces on `old_output`...
                                let old_output_workspaces = &w_per_o[old_output];
                                // find the workspaces on `new_output`...
                                let new_output_workspaces = &w_per_o[new_output];
                                // and swap them!
                                i3.run_command(format!(
                                    "{}, {}, workspace {}, workspace {}",
                                    old_output_workspaces
                                        .iter()
                                        .map(|w| format!(
                                            "workspace {w}, move workspace to output {new_output}"
                                        ))
                                        .join(", "),
                                    new_output_workspaces
                                        .iter()
                                        .map(|w| format!(
                                            "workspace {w}, move workspace to output {old_output}"
                                        ))
                                        .join(", "),
                                    old_output_workspaces
                                        .iter()
                                        .find(|w| w_by_num[*w].visible)
                                        .unwrap(),
                                    output.current_workspace.as_ref().unwrap(),
                                ))?;
                            } else if i3_shift {
                                preaction = format!("move container to output {}; ", output.name,);
                            }
                            i3.run_command(format!("{}focus output {}", preaction, output.name))?;
                        }
                        if let Event::KeyDown(k) = ui.event {
                            if k == base + 8 {
                                // Safety: not
                                unsafe {
                                    CURRENT_OUTPUT_HELD = Some(output.name.clone());
                                }
                            }
                        }
                        if let Event::KeyUp(k) = ui.event {
                            if k == base + 8 {
                                // Safety: also not
                                unsafe {
                                    CURRENT_OUTPUT_HELD = None;
                                }
                            }
                        }
                        base -= 10;
                    }

                    // shortcuts
                    ui.static_color(
                        87,
                        if process::Command::new("lsusb")
                            .arg("-d")
                            .arg("17a0:0304")
                            .stdout(process::Stdio::null())
                            .status()?
                            .success()
                        {
                            Color::simple(0)
                        } else if process::Command::new("pactl")
                            .arg("list")
                            .arg("short")
                            .arg("source-outputs")
                            .output()?
                            .stdout
                            .is_empty()
                        {
                            Color::simple(9)
                        } else {
                            Color::flashing(8, 8)
                        },
                    );
                    // if ui.impulse_button(88, Color::simple(92), Color::simple(92)) {
                    //     i3.run_command("exec --no-startup-id openrgb -p default")?;
                    // }
                    if ui.impulse_button(76, Color::simple(50), Color::simple(51)) {
                        i3.run_command("exec --no-startup-id autorandr -c")?;
                    }
                    ui.play_pause_button(78, Color::simple(21), Color::simple(23))?;
                    if ui.impulse_button(51, Color::simple(109), Color::simple(109)) {
                        i3.run_command("exec --no-startup-id lock")?;
                    }
                    // reading lamp
                    if ui.impulse_button(66, Color::simple(70), Color::simple(71)) {
                        i3.run_command("exec --no-startup-id director on b276c5a2-c2a6-457d-9637-b30c3175170f_1")?;
                    }
                    if ui.impulse_button(56, Color::simple(70), Color::simple(71)) {
                        i3.run_command("exec --no-startup-id director off b276c5a2-c2a6-457d-9637-b30c3175170f_1")?;
                    }
                    // window lamp
                    if ui.impulse_button(67, Color::simple(70), Color::simple(71)) {
                        i3.run_command("exec --no-startup-id director on 9920ff47-8524-47d5-8a11-780836e2e1c6_1")?;
                    }
                    if ui.impulse_button(57, Color::simple(70), Color::simple(71)) {
                        i3.run_command("exec --no-startup-id director off 9920ff47-8524-47d5-8a11-780836e2e1c6_1")?;
                    }
                    // my desk lamp
                    if ui.impulse_button(68, Color::simple(70), Color::simple(71)) {
                        i3.run_command("exec --no-startup-id director on 3b716c07-5829-4a44-b673-a1b5311bd051_1")?;
                    }
                    if ui.impulse_button(58, Color::simple(70), Color::simple(71)) {
                        i3.run_command("exec --no-startup-id director off 3b716c07-5829-4a44-b673-a1b5311bd051_1")?;
                    }
                    if ui.impulse_button(52, Color::simple(110), Color::simple(110)) {
                        i3.run_command("exec --no-startup-id xset dpms force off")?;
                        i3.run_command("exec --no-startup-id openrgb -p off")?;
                    }
                    if ui.impulse_button(54, Color::simple(10), Color::simple(9)) {
                        // i3.run_command("exec --no-startup-id pkill -USR1 gpu-screen-reco")?;
                        let _ = duct::cmd!("pkill", "-USR1", "gpu-screen-reco")
                            .unchecked()
                            .run();
                    }
                    // soundboard
                    if ui.toggle_button(55, Color::simple(118), Color::simple(119)) {
                        if ui.impulse_button(44, Color::simple(18), Color::simple(19)) {
                            i3.run_command("exec --no-startup-id paplay --device mixin-to-mix-sink /home/ash/Downloads/boowomp.mp3")?;
                        }
                        if ui.impulse_button(34, Color::simple(18), Color::simple(19)) {
                            i3.run_command("exec --no-startup-id paplay --device mixin-to-mix-sink /home/ash/Downloads/trombone.mp3")?;
                        }
                        if ui.impulse_button(31, Color::simple(5), Color::simple(6)) {
                            i3.run_command("exec --no-startup-id paplay --device mixin-to-mix-sink /home/ash/Downloads/metal-pipe.mp3")?;
                        }
                        if ui.impulse_button(45, Color::simple(14), Color::simple(15)) {
                            i3.run_command("exec --no-startup-id paplay --device mixin-to-mix-sink /home/ash/Downloads/bonk.mp3")?;
                        }
                        if ui.impulse_button(46, Color::simple(104), Color::simple(112)) {
                            i3.run_command("exec --no-startup-id paplay --device mixin-to-mix-sink /home/ash/Downloads/cave1.ogg")?;
                        }
                        if ui.impulse_button(47, Color::simple(49), Color::simple(50)) {
                            i3.run_command("exec --no-startup-id paplay --device mixin-to-mix-sink /home/ash/Downloads/discord-join.mp3")?;
                        }
                        if ui.impulse_button(48, Color::simple(49), Color::simple(50)) {
                            i3.run_command("exec --no-startup-id paplay --device mixin-to-mix-sink /home/ash/Downloads/discord-ping.mp3")?;
                        }
                        if ui.impulse_button(33, Color::simple(79), Color::simple(66)) {
                            i3.run_command("exec --no-startup-id paplay --device mixin-to-mix-sink /home/ash/Downloads/windows-disconnect.mp3")?;
                        }
                        if ui.impulse_button(43, Color::simple(79), Color::simple(66)) {
                            i3.run_command("exec --no-startup-id paplay --device mixin-to-mix-sink /home/ash/Downloads/windows-connect.mp3")?;
                        }
                    }

                    // playback bar
                    // let cmd_position = process::Command::new("playerctl")
                    //     .arg("position")
                    //     .output()?;
                    // let position: Option<u64> = cmd_position.status.success().then(|| {
                    //     (1000000.0
                    //         * std::str::from_utf8(&cmd_position.stdout)
                    //             .unwrap()
                    //             .trim_end()
                    //             .parse()
                    //             .unwrap_or(0.0)) as u64
                    // });
                    // let cmd_length = process::Command::new("playerctl")
                    //     .arg("metadata")
                    //     .arg("mpris:length")
                    //     .output()?;
                    // let length: Option<u64> = cmd_length.status.success().then(|| {
                    //     std::str::from_utf8(&cmd_length.stdout)
                    //         .unwrap()
                    //         .trim_end()
                    //         .parse()
                    //         .unwrap_or(0)
                    // });
                    // if let (Some(pos), Some(len)) = (position, length) {
                    //     for (i, key) in rect(31, 38).enumerate() {
                    //         ui.static_color(
                    //             key,
                    //             Color::simple(if pos > i as u64 * len / 8 { 49 } else { 51 }),
                    //         );
                    //     }
                    // }

                    // piano
                    // calculated with rink:
                    // > 27.5 * (2**3)
                    // 220  (dimensionless)
                    // > 220 * ((2 ** (1/12)) ** 0)
                    // approx. 220  (dimensionless)
                    // > 220 * ((2 ** (1/12)) ** 1)
                    // approx. 233.0818  (dimensionless)
                    // > 220 * ((2 ** (1/12)) ** 2)
                    // approx. 246.9416  (dimensionless)
                    // ...and so on up to 12 (= 440)
                    // white notes: 0, 2, 3, 5, 7, 8, 10:
                    // 220.0, 246.9416, 261.6255, 293.6647, 329.6275, 349.2282, 391.9954,
                    // 440.0, 493.8833, 523.2511, 587.3295, 659.2551, 698.4564, 783.9908
                    // black notes: 1, (gap), 4, 6, (gap), 9, 11:
                    // 233.0818, None, 277.1826, 311.1269, None, 369.9944, 415.3046,
                    // 466.1637, None, 554.3652, 622.2539, None, 739.9888, 830.6093
                    for (i, freq) in //[262, 294, 330, 349, 392, 440, 494, 524]
                        [
                            261.6255, 293.6647, 329.6275, 349.2282, 391.9954, 440.0, 493.8833,
                            523.2511,
                        ]
                        .into_iter()
                        .enumerate()
                    {
                        if ui.holdable_button(
                            (i + 11) as Key,
                            Color::Simple(SimpleColor::Static(92)),
                            Color::Simple(SimpleColor::Static(91)),
                        ) {
                            let x: &mut HashMap<usize, NoteState> = &mut audio_state.lock().notes;
                            x.entry(i + 100)
                                .or_insert_with(|| NoteState::new(freq as f32))
                                .input = true;
                        } else {
                            let x: &mut HashMap<usize, NoteState> = &mut audio_state.lock().notes;
                            x.entry(i + 100)
                                .or_insert_with(|| NoteState::new(freq as f32))
                                .input = false;
                        }
                    }
                    for (i, freq) in [
                        Some(277.1826),
                        Some(311.1269),
                        None,
                        Some(369.9944),
                        Some(415.3046),
                        Some(466.1637),
                    ]
                    .into_iter()
                    .enumerate()
                    {
                        if let Some(freq) = freq {
                            if ui.holdable_button(
                                (i + 22) as Key,
                                Color::Simple(SimpleColor::Static(94)),
                                Color::Simple(SimpleColor::Static(93)),
                            ) {
                                let x: &mut HashMap<usize, NoteState> =
                                    &mut audio_state.lock().notes;
                                x.entry(i + 200)
                                    .or_insert_with(|| NoteState::new(freq as f32))
                                    .input = true;
                            } else {
                                let x: &mut HashMap<usize, NoteState> =
                                    &mut audio_state.lock().notes;
                                x.entry(i + 200)
                                    .or_insert_with(|| NoteState::new(freq as f32))
                                    .input = false;
                            }
                        }
                    }
                }
                1 => {
                    let base = u8::try_from(ui.counter_buttons::<2>(93) * 64).unwrap();
                    for (i, key) in rect(11, 88).enumerate() {
                        let color = base + i as u8;
                        ui.info_button(
                            key,
                            Color::Simple(SimpleColor::Static(color)),
                            &color.to_string(),
                        );
                    }
                }
                2 => {
                    // "fuck up" button
                    audio_state.lock().glitch_mode =
                        ui.toggle_button(89, Color::simple(61), Color::pulsing(60));
                    audio_state.lock().forget_copysign =
                        ui.toggle_button(79, Color::simple(8), Color::pulsing(9));

                    if ui.toggle_button(69, Color::simple(1), Color::simple(1)) {
                        // JI lattice
                        for row in 1..=8 {
                            for col in 1..=7 {
                                let fifths = f64::powf(3./2., row as f64 - 1.);
                                let thirds = f64::powf(5./4., col as f64 - 1.);
                                let freq = 55. * fifths * thirds;
                                audio_state.lock().notes.entry(10_000 + (row as usize) * 10 + (col as usize))
                                    .or_insert_with(|| NoteState::new(freq as f32))
                                    .input = ui.holdable_button(row * 10 + col, Color::simple(1), Color::simple(2))
                            }
                        }
                    } else {
                        // regular piano keyboard
                        for (row, freq_mult) in [0.5, 1.0, 2.0, 4.0].into_iter().enumerate() {
                            for (i, freq) in [
                                261.6255, 293.6647, 329.6275, 349.2282, 391.9954, 440.0, 493.8833,
                                523.2511,
                            ]
                                .into_iter()
                                .enumerate()
                            {
                                if ui.holdable_button(
                                    (i + 11 + (row * 20)) as Key,
                                    Color::Simple(SimpleColor::Static(92)),
                                    Color::Simple(SimpleColor::Static(91)),
                                ) {
                                    let x: &mut HashMap<usize, NoteState> =
                                        &mut audio_state.lock().notes;
                                    x.entry(i + 1000 + (row * 100))
                                        .or_insert_with(|| NoteState::new(freq as f32 * freq_mult))
                                        .input = true;
                                } else {
                                    let x: &mut HashMap<usize, NoteState> =
                                        &mut audio_state.lock().notes;
                                    x.entry(i + 1000 + (row * 100))
                                        .or_insert_with(|| NoteState::new(freq as f32 * freq_mult))
                                        .input = false;
                                }
                            }
                            for (i, freq) in [
                                Some(277.1826),
                                Some(311.1269),
                                None,
                                Some(369.9944),
                                Some(415.3046),
                                Some(466.1637),
                            ]
                                .into_iter()
                                .enumerate()
                            {
                                if let Some(freq) = freq {
                                    if ui.holdable_button(
                                        (i + 22 + (row * 20)) as Key,
                                        Color::Simple(SimpleColor::Static(94)),
                                        Color::Simple(SimpleColor::Static(93)),
                                    ) {
                                        let x: &mut HashMap<usize, NoteState> =
                                            &mut audio_state.lock().notes;
                                        x.entry(i + 2000 + (row * 100))
                                            .or_insert_with(|| NoteState::new(freq as f32 * freq_mult))
                                            .input = true;
                                    } else {
                                        let x: &mut HashMap<usize, NoteState> =
                                            &mut audio_state.lock().notes;
                                        x.entry(i + 2000 + (row * 100))
                                            .or_insert_with(|| NoteState::new(freq as f32 * freq_mult))
                                            .input = false;
                                    }
                                }
                            }
                        }
                    }

                }
                3 => {
                    // "L", "D"
                    for key in [81, 71, 61, 51, 52, 86, 87, 76, 78, 66, 68, 56, 57] {
                        ui.static_color(key, Color::Simple(SimpleColor::Static(40)));
                    }
                    // "E"
                    for key in [83, 84, 85, 73, 74, 63, 53, 54, 55] {
                        ui.static_color(key, Color::Simple(SimpleColor::Static(113)));
                    }
                    ui.led_slider(31);
                    ui.exit_button(18);
                }
                _ => unreachable!(),
            }
        }
        // redraw
        launchpad.full_update(&fb)?;
    }

    animations::shutdown(&mut launchpad).wrap_err("couldn't display shutdown animation")?;

    Ok(())
}
