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

use eyre::WrapErr;
use i3_ipc::{Connect, I3};
use itertools::Itertools;

use lp::{coords_to_key, key_to_coords, rect, Color, Command, ComplexColor, Key, Launchpad, Message, SimpleColor, Direction::{Up, Right, Down, Left}, line};

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
                    if ui.impulse_button(76, Color::simple(50), Color::simple(51)) {
                        i3.run_command("exec --no-startup-id autorandr -c")?;
                    }
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
                    } else {
                        ui.play_pause_button(41, Color::simple(21), Color::simple(23))?;
                        // playback bar
                        let cmd_position = process::Command::new("playerctl")
                            .arg("position")
                            .output()?;
                        let position: Option<u64> = cmd_position.status.success().then(|| {
                            (1000000.0 // microseconds in a second
                                * std::str::from_utf8(&cmd_position.stdout)
                                .unwrap()
                                .trim_end()
                                .parse()
                                .unwrap_or(0.0)) as u64
                        });
                        let cmd_length = process::Command::new("playerctl")
                            .arg("metadata")
                            .arg("mpris:length")
                            .output()?;
                        let length: Option<u64> = cmd_length.status.success().then(|| {
                            std::str::from_utf8(&cmd_length.stdout)
                                .unwrap()
                                .trim_end()
                                .parse()
                                .unwrap_or(0)
                        });
                        if let (Some(pos), Some(len)) = (position, length) {
                            const KEYS: u64 = 19;
                            let mut offset = 0;
                            for (start, direction, length) in [
                                (42, Right, 7),
                                (38, Down, 2),
                                (18, Left, 8),
                                (21, Up, 2),
                            ] {
                                for (i, key) in line(start, direction, length).enumerate() {
                                    let start = (i as u64 + offset) * len / KEYS;
                                    let end = (i as u64 + offset + 1) * len / KEYS;
                                    if ui.impulse_button(key, Color::simple(if pos >= start && pos < end { 33 } else { 35 }), Color::simple(32)) {
                                        process::Command::new("playerctl").arg("position").arg((start as f64 / 1000000.0).to_string()).status()?;
                                    }
                                }
                                offset += length as u64;
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
                    // TODO
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
