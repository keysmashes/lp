mod ui;

use std::collections::HashMap;
use std::process;
use std::{
    collections::hash_map::DefaultHasher,
    hash::{Hash, Hasher},
    io::{BufRead, BufReader},
    sync::{atomic::AtomicBool, mpsc, Arc},
    thread,
    time::Duration,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use eyre::WrapErr;
use i3_ipc::{Connect, I3};
use itertools::Itertools;
use parking_lot::Mutex;
// use rdev::Key::*;

use lp::{
    coords_to_key, key_to_coords, rect, Color, Command, ComplexColor, Key, Launchpad, Message,
    SimpleColor,
};

// https://gist.github.com/sug0/b5eb2c58be74f7cda230b8c1e1994670
fn fast_sin(mut x: f64) -> f64 {
    let (mut y, mut z) = (x, x);
    z *= 0.3183098861837907;
    z += 6755399441055744.0;
    let mut k = unsafe {
        let p: *const i32 = &z as *const _ as *const _;
        *p
    };
    z = k as f64;
    z *= 3.1415926535897932;
    x -= z;
    y *= x;
    z = 0.0073524681968701;
    z *= y;
    z -= 0.1652891139701474;
    z *= y;
    z += 0.9996919862959676;
    x *= z;
    k &= 1;
    k += k;
    z = k as f64;
    z *= x;
    x -= z;
    x
}

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

mod animations {
    use itertools::Itertools;
    use std::{
        iter, thread,
        time::{Duration, Instant},
    };

    use super::{coords_to_key, key_to_coords, Command, Key, Launchpad, SimpleColor};

    const TRANS_BLUE: u8 = 37;
    const TRANS_PINK: u8 = 52;
    const TRANS_WHITE: u8 = 3;

    fn up_left_from(start: u8) -> impl Iterator<Item = Key> {
        iter::successors(Some(key_to_coords(start)), |(x, y)| {
            if *x == 1 || *y == 9 {
                None
            } else {
                Some((*x - 1, *y + 1))
            }
        })
        .map(|(x, y)| coords_to_key(x, y))
    }
    fn along_bottom_right() -> impl Iterator<Item = Key> {
        (11..19).chain((19..=99).step_by(10))
    }
    fn left_from(start: u8) -> impl Iterator<Item = Key> {
        iter::successors(Some(key_to_coords(start)), |(x, y)| {
            if *x == 1 {
                None
            } else {
                Some((*x - 1, *y))
            }
        })
        .map(|(x, y)| coords_to_key(x, y))
    }
    fn down_from(start: u8) -> impl Iterator<Item = Key> {
        iter::successors(Some(key_to_coords(start)), |(x, y)| {
            if *y == 1 {
                None
            } else {
                Some((*x, *y - 1))
            }
        })
        .map(|(x, y)| coords_to_key(x, y))
    }
    fn right_from(start: u8) -> impl Iterator<Item = Key> {
        iter::successors(Some(key_to_coords(start)), |(x, y)| {
            if *x == 9 {
                None
            } else {
                Some((*x + 1, *y))
            }
        })
        .map(|(x, y)| coords_to_key(x, y))
    }
    fn up_from(start: u8) -> impl Iterator<Item = Key> {
        iter::successors(Some(key_to_coords(start)), |(x, y)| {
            if *y == 9 {
                None
            } else {
                Some((*x, *y + 1))
            }
        })
        .map(|(x, y)| coords_to_key(x, y))
    }

    pub(crate) fn startup(launchpad: &mut Launchpad) -> eyre::Result<()> {
        const STRIPES: &[u8] = &[
            0,
            TRANS_BLUE,
            TRANS_BLUE,
            TRANS_PINK,
            TRANS_PINK,
            TRANS_WHITE,
            TRANS_WHITE,
            TRANS_PINK,
            TRANS_PINK,
            TRANS_BLUE,
            TRANS_BLUE,
        ];
        const DELAY: Duration = Duration::from_millis(50);

        let mut it = iter::repeat::<Option<Key>>(None)
            .take(STRIPES.len() - 1)
            .chain(
                along_bottom_right()
                    .collect_vec()
                    .into_iter()
                    .rev()
                    .map(Option::Some),
            )
            .multipeek();
        loop {
            let t = Instant::now();
            // draw the stripes in reverse order, as it were
            for color in STRIPES {
                if let Some(Some(start)) = it.peek() {
                    for key in up_left_from(*start) {
                        launchpad.send(&Command::KeyOn(key, SimpleColor::Static(*color)))?;
                    }
                }
            }
            if it.next().is_none() {
                break;
            }
            thread::sleep(DELAY.saturating_sub(t.elapsed()));
        }

        Ok(())
    }

    pub(crate) fn shutdown(launchpad: &mut Launchpad) -> eyre::Result<()> {
        // clear the display, there may be garbage on it
        for x in 1..=9 {
            for y in 1..=9 {
                launchpad.send(&Command::KeyOff(coords_to_key(x, y)))?;
            }
        }

        const STRIPES: &[u8] = &[
            0,
            TRANS_BLUE,
            TRANS_PINK,
            TRANS_WHITE,
            TRANS_PINK,
            TRANS_BLUE,
        ];
        const DELAY: Duration = Duration::from_millis(50);

        let mut it = iter::repeat::<Option<(u8, Key)>>(None)
            .take(STRIPES.len() - 1)
            .chain(
                [
                    (1, 99),
                    (2, 89),
                    (3, 79),
                    (4, 69),
                    (4, 59),
                    (3, 58),
                    (2, 57),
                    (1, 56),
                    (1, 55),
                ]
                .into_iter()
                .map(Option::Some),
            )
            .take(9 + STRIPES.len() - 1)
            .multipeek();
        loop {
            let t = Instant::now();
            // draw the stripes in reverse order, as it were
            for color in STRIPES {
                if let Some(Some((n, start))) = it.peek() {
                    for key in up_left_from(*start).take(*n as usize) {
                        launchpad.send(&Command::KeyOn(key, SimpleColor::Static(*color)))?;
                        let mut x = 0;
                        let mut y = 0;
                        (x, y) = key_to_coords(key);
                        for _ in 0..3 {
                            (x, y) = ((-(y as i8 - 5) + 5) as u8, x);
                            launchpad.send(&Command::KeyOn(
                                coords_to_key(x, y),
                                SimpleColor::Static(*color),
                            ))?;
                        }
                    }
                }
            }
            if it.next().is_none() {
                break;
            }
            thread::sleep(DELAY.saturating_sub(t.elapsed()));
        }

        Ok(())
    }

    // just flash the entire launchpad orange
    // TODO: this should be an animation emanating from the square responsible for the alert
    pub(crate) fn alert(launchpad: &mut Launchpad, focus: Option<u8>) -> eyre::Result<()> {
        let is_real_focus = focus.is_some();
        let focus = focus.unwrap_or(55);
        let (focus_x, focus_y) = key_to_coords(focus);
        // let (focus_x, focus_y) = key_to_coords(55);
        const DELAY: Duration = Duration::from_millis(50);

        // first, clear the display
        for x in 1..=9 {
            for y in 1..=9 {
                launchpad.send(&Command::KeyOff(coords_to_key(x, y)))?;
            }
        }
        // next, pulse the focused key (it will remain this way throughout)
        launchpad.send(&Command::KeyOn(
            coords_to_key(focus_x, focus_y),
            SimpleColor::Pulsing(9),
        ))?;
        // launchpad.send(&Command::KeyOn(coords_to_key(focus_x, focus_y), SimpleColor::Static(9)))?;

        fn light(launchpad: &mut Launchpad, x: i8, y: i8) -> eyre::Result<()> {
            if (1..=9).contains(&x) && (1..=9).contains(&y) {
                launchpad.send(&Command::KeyOn(
                    coords_to_key(x as u8, y as u8),
                    SimpleColor::Static(9),
                ))?;
            }
            Ok(())
        }
        fn extinguish(launchpad: &mut Launchpad, x: i8, y: i8) -> eyre::Result<()> {
            if (1..=9).contains(&x) && (1..=9).contains(&y) {
                launchpad.send(&Command::KeyOff(coords_to_key(x as u8, y as u8)))?;
            }
            Ok(())
        }

        // phase 1: expand from the focus
        let mut top_bound = focus_y;
        let mut bottom_bound = focus_y;
        let mut left_bound = focus_x;
        let mut right_bound = focus_x;
        let mut up = up_from(focus).skip(1);
        let mut down = down_from(focus).skip(1);
        let mut left = left_from(focus).skip(1);
        let mut right = right_from(focus).skip(1);
        loop {
            thread::sleep(DELAY);
            match (up.next(), down.next(), left.next(), right.next()) {
                (None, None, None, None) => break,
                (u, d, l, r) => {
                    top_bound = u.map(|k| key_to_coords(k).1).unwrap_or(top_bound);
                    bottom_bound = d.map(|k| key_to_coords(k).1).unwrap_or(bottom_bound);
                    left_bound = l.map(|k| key_to_coords(k).0).unwrap_or(left_bound);
                    right_bound = r.map(|k| key_to_coords(k).0).unwrap_or(right_bound);
                    // println!("expanding: filling from ({}, {}) to ({}, {})", left_bound, top_bound, right_bound, bottom_bound);
                    for x in left_bound..=right_bound {
                        for y in bottom_bound..=top_bound {
                            if x != focus_x || y != focus_y {
                                launchpad.send(&Command::KeyOn(
                                    coords_to_key(x, y),
                                    SimpleColor::Static(9),
                                ))?;
                            }
                        }
                    }
                }
            }
        }
        // for (i, top_left) in up_left_from(coords_to_key(focus_x, focus_y)).enumerate().skip(1) {
        //     thread::sleep(DELAY);
        //     // println!("expanding: {:?}, top_left {:?}", i, top_left);
        //     for x_off in 0..=(i*2) {
        //         for y_off in 0..=(i*2) {
        //             let (x, y) = (key_to_coords(top_left).0 + x_off as u8, key_to_coords(top_left).1 - y_off as u8);
        //             if x != focus_x || y != focus_y {
        //                 // launchpad.send(&Command::KeyOn(coords_to_key(x, y), SimpleColor::Static(9)))?;
        //                 light(launchpad, x as i8, y as i8)?;
        //             }
        //         }
        //     }
        // }

        // phase 2: wait a bit
        thread::sleep(Duration::from_millis(900));

        // phase 3: contract back towards the focus
        let mut top_bound = focus_y;
        let mut bottom_bound = focus_y;
        let mut left_bound = focus_x;
        let mut right_bound = focus_x;
        let mut up = up_from(focus).skip(1);
        let mut down = down_from(focus).skip(1);
        let mut left = left_from(focus).skip(1);
        let mut right = right_from(focus).skip(1);
        // let mut commands = Vec::new();
        let mut bounds = Vec::new();
        loop {
            match (up.next(), down.next(), left.next(), right.next()) {
                (None, None, None, None) => break,
                (u, d, l, r) => {
                    bounds.push((u, d, l, r));
                    // top_bound = u.map(|k| key_to_coords(k).1).unwrap_or(top_bound);
                    // bottom_bound = d.map(|k| key_to_coords(k).1).unwrap_or(bottom_bound);
                    // left_bound = l.map(|k| key_to_coords(k).0).unwrap_or(left_bound);
                    // right_bound = r.map(|k| key_to_coords(k).0).unwrap_or(right_bound);
                    // let mut batch = Vec::new();
                    // if l.is_some() && r.is_some() {
                    //     for x in left_bound..=right_bound {
                    //         if d.is_some() {
                    //             batch.push(Command::KeyOff(coords_to_key(x, bottom_bound)));
                    //         }
                    //         if u.is_some() {
                    //             batch.push(Command::KeyOff(coords_to_key(x, top_bound)));
                    //         }
                    //     }
                    // }
                    // if d.is_some() && u.is_some() {
                    //     for y in bottom_bound..=top_bound {
                    //         if l.is_some() {
                    //             batch.push(Command::KeyOff(coords_to_key(left_bound, y)));
                    //         }
                    //         if r.is_some() {
                    //             batch.push(Command::KeyOff(coords_to_key(right_bound, y)));
                    //         }
                    //     }
                    // }
                    // commands.push(batch);
                }
            }
        }
        for (u, d, l, r) in bounds.into_iter().rev() {
            if let Some(u) = u {
                for x in 1..=9 {
                    launchpad.send(&Command::KeyOff(coords_to_key(x, key_to_coords(u).1)))?;
                }
            }
            if let Some(d) = d {
                for x in 1..=9 {
                    launchpad.send(&Command::KeyOff(coords_to_key(x, key_to_coords(d).1)))?;
                }
            }
            if let Some(l) = l {
                for y in 1..=9 {
                    launchpad.send(&Command::KeyOff(coords_to_key(key_to_coords(l).0, y)))?;
                }
            }
            if let Some(r) = r {
                for y in 1..=9 {
                    launchpad.send(&Command::KeyOff(coords_to_key(key_to_coords(r).0, y)))?;
                }
            }
            thread::sleep(DELAY);
        }
        // for batch in commands.into_iter().rev() {
        //     dbg!(&batch);
        //     for command in batch {
        //         if !matches!(command, Command::KeyOff(k) if k == focus) {
        //             launchpad.send(&command)?;
        //         }
        //     }
        //     thread::sleep(DELAY);
        // }
        // for (i, top_left) in up_left_from(coords_to_key(focus_x, focus_y)).enumerate().skip(1).collect_vec().into_iter().rev() {
        //     // println!("contracting: {:?}, top_left {:?}", i, top_left);
        //     for x_off in 0..=(i*2) {
        //         for y_off in 0..=(i*2) {
        //             let (x, y) = (key_to_coords(top_left).0 + x_off as u8, key_to_coords(top_left).1 - y_off as u8);
        //             if (x != focus_x || y != focus_y) && (x_off == 0 || x_off == i*2 || y_off == 0 || y_off == i*2) {
        //                 // launchpad.send(&Command::KeyOff(coords_to_key(x, y)))?;
        //                 extinguish(launchpad, x as i8, y as i8)?;
        //             }
        //         }
        //     }
        //     thread::sleep(DELAY);
        // }

        // clean up the pulsing focus (unless it's wanted by the caller)
        if !is_real_focus {
            launchpad.send(&Command::KeyOff(coords_to_key(focus_x, focus_y)))?;
        }

        Ok(())
    }
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

// #[derive(Clone, Copy, Debug)]
// enum Direction {
//     Right,
//     Down,
// }
//
// impl Direction {
//     fn iter_from(&self, start: Key) -> impl Iterator<Item = Key> + '_ {
//         iter::successors(Some(key_to_coords(start)), |(x, y)| match *self {
//             Direction::Right => {
//                 if *x == 9 {
//                     None
//                 } else {
//                     Some((*x + 1, *y))
//                 }
//             }
//             Direction::Down => {
//                 if *y == 1 {
//                     None
//                 } else {
//                     Some((*x, *y - 1))
//                 }
//             }
//         })
//         .map(|(x, y)| coords_to_key(x, y))
//     }
// }

// fn simulate_press(keys: &[rdev::Key]) -> eyre::Result<()> {
//     for key in keys {
//         rdev::simulate(&rdev::EventType::KeyPress(*key))?;
//         thread::sleep(Duration::from_millis(10));
//     }
//     Ok(())
// }

// fn simulate_release(keys: &[rdev::Key]) -> eyre::Result<()> {
//     for key in keys.into_iter().rev() {
//         rdev::simulate(&rdev::EventType::KeyRelease(*key))?;
//         thread::sleep(Duration::from_millis(10));
//     }
//     Ok(())
// }

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
            let mut i3 = I3Stream::conn_sub(&[Workspace, Output]).unwrap();
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

    // thread::Builder::new()
    //     .name("lp stdin handler".into())
    //     .spawn(move || {
    //         let mut x = String::new();
    //         std::io::stdin().read_line(&mut x).unwrap();
    //         tx.send(Event::Exit).unwrap();
    //     })?;

    launchpad.send(&Command::SetAwake(true))?;
    animations::startup(&mut launchpad).wrap_err("couldn't display startup animation")?;
    // discard events that arrived during the startup animation
    for _ in rx.try_iter() {}
    tx.send(Event::Redraw)?;

    // let mut keypad = [false; 100];
    let mut keypad = HashMap::with_capacity(81);
    for key in rect(11, 88) {
        keypad.insert(key, false);
    }

    let mut fb = HashMap::with_capacity(81);
    for key in rect(11, 99) {
        fb.insert(key, Color::Simple(SimpleColor::Static(0)));
    }

    // let mixer = Arc::new(Mutex::new(usfx::Mixer::default()));
    // mixer.play(sample);
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
    }
    fn get_audio_frame_static() -> f32 {
        static mut clock: f32 = 0.0;
        unsafe {
            clock += 1.0;
            if clock >= 44100.0 {
                clock = 0.0;
            }
            let period = clock / 44100.0;
            (440.0 * std::f32::consts::TAU * period).sin() * 0.2
        }
    }
    fn get_audio_frame(audio_state: &mut AudioState) -> f32 {
        let mut value: f32 = 0.0;
        for (_, state) in audio_state.notes.iter_mut() {
            if state.input {
                state.volume = 1.0;
            }
            if state.volume > 0.0 {
                state.clock += 1.0;
                if state.clock >= 44100.0 {
                    state.clock = 0.0;
                }
                let period = state.clock / 44100.0;
                let sample = (state.freq * std::f32::consts::TAU * period * 2.0).sin();
                // let sample = fast_sin((state.freq * std::f32::consts::TAU * period * 2.0) as f64);
                value += sample as f32 * 0.2 * state.volume;
                state.volume -= 0.0004;
            } else {
                state.clock = 0.0;
            }
        }
        value
        // if audio_state.active {
        //     audio_state.clock += 1.0;
        //     let period = audio_state.clock / 44100.0;
        //     let sample = (440.0 * std::f32::consts::TAU * period).sin();
        //     return sample * 0.2;
        // } else {
        //     audio_state.clock = 0.0;
        // }
        // 0.0
    }
    let audio_state = Arc::new(Mutex::new(AudioState {
        // active: false,
        // clock: 0.0,
        notes: HashMap::new(),
    }));
    let stream = device.build_output_stream(
        &cpal::StreamConfig {
            channels: 1,
            sample_rate: cpal::SampleRate(44100),
            buffer_size: cpal::BufferSize::Default,
            // buffer_size: cpal::BufferSize::Fixed(2048 * 8),
        },
        {
            // let mixer = mixer.clone();
            // move |mut data: &mut [f32], _info| {
            //     mixer.lock().generate(data);
            // }
            // https://github.com/0xC45/simple-synth/blob/42611692157830df9c17de10dd20abb4ee2806e1/src/main.rs#L236
            let state = audio_state.clone();
            move |data: &mut [f32], _info| {
                for frame in data.chunks_mut(config.channels() as usize) {
                    // let v = cpal::Sample::from::<f32>(&get_audio_frame());
                    let v = get_audio_frame(&mut state.lock());
                    // let v = get_audio_frame_static();
                    for value in frame.iter_mut() {
                        *value = v;
                    }
                }
            }
        },
        |error| Err(error).wrap_err("uhh").unwrap(),
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
        if ui.awake(19, Color::Simple(SimpleColor::Static(47))) {
            let tab = ui.tabs::<4>(95);
            // if tab == 1 || tab == 2 {
            //     for key in rect(29, 89) {
            //         ui.palette_button(key);
            //     }
            // }
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
                    // for workspace_num in output_base..output_base + 5 {
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
                                    // let mut color =
                                    //     I3_COLORS[hasher.finish() as usize % I3_COLORS.len()];
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
                    for (i, output) in outputs
                        .iter()
                        .filter(|o| o.active)
                        .sorted_by(|a, b| a.rect.x.cmp(&b.rect.x))
                        .sorted_by(|a, b| a.rect.y.cmp(&b.rect.y))
                        .enumerate()
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
                                // let mut color =
                                //     I3_COLORS[hasher.finish() as usize % I3_COLORS.len()];
                                let mut color = if let Some(c) = output_colors.get(&*w.output) {
                                    *c
                                } else {
                                    I3_COLORS[hasher.finish() as usize % I3_COLORS.len()]
                                };
                                if !w.focused {
                                    color += 2;
                                }
                                color
                                // if w.focused {
                                //     // 21
                                // } else {
                                //     1
                                // }
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
                        // for output_num in w_per_o[&output.name].iter() {
                        //     let color = Color::simple({
                        //         let w = &w_by_num[output_num];
                        //         if w.focused {
                        //             21
                        //         } else if w.urgent {
                        //             9
                        //         } else if w.visible {
                        //             3
                        //         } else {
                        //             1
                        //         }
                        //     });
                        //     // TODO: yuck (specifically, the `as`)
                        //     if ui.impulse_button(base + *output_num as u8 % 5, color, color) {
                        //         i3.run_command(format!("workspace number {}", output_num))?;
                        //     }
                        // }
                        base -= 10;
                    }

                    // shortcuts
                    ui.static_color(
                        88,
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
                            Color::flashing(9, 0)
                        },
                    );
                    // ui.static_color(88, Color::simple(
                    //     if process::Command::new("pactl")
                    //         .arg("list")
                    //         .arg("short")
                    //         .arg("source-outputs")
                    //         .output()?
                    //         .stdout.is_empty() { 1 } else { 9 }
                    // ));
                    // match ui.press_release_button(68, Color::simple(92), Color::simple(92)) {
                    //     Some(true) => simulate_press(&[MetaLeft, KeyX])?,
                    //     // Some(true) => simulate_press(&[Alt, KeyX])?,
                    //     Some(false) => simulate_release(&[MetaLeft, KeyX])?,
                    //     // Some(false) => simulate_release(&[Alt, KeyX])?,
                    //     None => {}
                    // }
                    if ui.impulse_button(68, Color::simple(92), Color::simple(92)) {
                        i3.run_command("exec --no-startup-id i3-workspace-swap")?;
                    }
                    ui.play_pause_button(58, Color::simple(21), Color::simple(23))?;
                    if ui.impulse_button(51, Color::simple(109), Color::simple(109)) {
                        // was color 61
                        // simulate_press(&[MetaLeft, ShiftLeft, KeyF])?;
                        // simulate_press(&[Alt, ShiftLeft, KeyF])?;
                        // thread::sleep(Duration::from_millis(10));
                        // simulate_release(&[MetaLeft, ShiftLeft, KeyF])?;
                        // simulate_release(&[Alt, ShiftLeft, KeyF])?;
                        i3.run_command("exec --no-startup-id lock")?;
                    }
                    if ui.impulse_button(66, Color::simple(70), Color::simple(71)) {
                        i3.run_command("exec --no-startup-id iot big-lamp on")?;
                    }
                    if ui.impulse_button(56, Color::simple(70), Color::simple(71)) {
                        i3.run_command("exec --no-startup-id iot big-lamp off")?;
                    }
                    if ui.impulse_button(67, Color::simple(70), Color::simple(71)) {
                        i3.run_command("exec --no-startup-id iot tall-lamp on")?;
                    }
                    if ui.impulse_button(57, Color::simple(70), Color::simple(71)) {
                        i3.run_command("exec --no-startup-id iot tall-lamp off")?;
                    }
                    if ui.impulse_button(52, Color::simple(110), Color::simple(110)) {
                        i3.run_command("exec --no-startup-id xset dpms force off")?;
                    }
                    // soundboard
                    if ui.toggle_button(55, Color::simple(118), Color::simple(119)) {
                        if ui.impulse_button(44, Color::simple(18), Color::simple(19)) {
                            i3.run_command("exec --no-startup-id paplay --device mixin-to-mix-sink /home/ash/Downloads/boowomp.mp3")?;
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
                    // let mut sample = usfx::Sample::default();
                    // sample.osc_type(usfx::OscillatorType::Sine);
                    // sample.env_attack(0.02);
                    // sample.env_decay(0.05);
                    // sample.env_sustain(0.2);
                    // sample.env_release(0.5);
                    // sample.dis_crunch(0.5);
                    // sample.dis_drive(0.9);
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
                            // sample.osc_frequency(freq);
                            // mixer.lock().play(sample);
                            // TODO: replace with cpal thing
                            // audio_state.lock().active = true;
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
                    for (i, freq) in
                        //[Some(277), Some(311), None, Some(370), Some(415), Some(466)]
                        [
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
                                // sample.osc_frequency(freq);
                                // mixer.lock().play(sample);
                                // TODO: replace with cpal thing
                                // audio_state.lock().active = false;
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
                            &(color).to_string(),
                        );
                    }
                }
                2 => {
                    // for key in rect(11, 88) {
                    //     ui.toggle_button(
                    //         key,
                    //         Color::Simple(SimpleColor::Static(0)),
                    //         Color::Simple(SimpleColor::Static(20)),
                    //     );
                    // }
                    for (row, freq_mult) in [0.5, 1.0, 2.0, 4.0].into_iter().enumerate() {
                        for (i, freq) in //[262, 294, 330, 349, 392, 440, 494, 524]
                            [
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
                                // sample.osc_frequency(freq);
                                // mixer.lock().play(sample);
                                // TODO: replace with cpal thing
                                // audio_state.lock().active = true;
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
                        for (i, freq) in
                            //[Some(277), Some(311), None, Some(370), Some(415), Some(466)]
                            [
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
                                    // sample.osc_frequency(freq);
                                    // mixer.lock().play(sample);
                                    // TODO: replace with cpal thing
                                    // audio_state.lock().active = false;
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
