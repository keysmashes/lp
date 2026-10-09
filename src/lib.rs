use std::collections::HashMap;
use std::io::{self, Write};
use std::iter;
use midir::{ConnectError, MidiInput, MidiOutput};
use thiserror::Error;

pub struct Launchpad {
    out_con: midir::MidiOutputConnection,
    _in_con: midir::MidiInputConnection<()>,
    send_buf: Vec<u8>,
    complex_color_buf: Vec<(Key, ComplexColor)>,
    current: HashMap<Key, Color>,
}

#[derive(Debug, Error)]
pub enum ConnectionError {
    #[error("couldn't initialise MIDI backend")]
    MidiInitError(#[from] midir::InitError),
    #[error("Launchpad was not found")]
    NotFoundError,
    #[error("error connecting to the Launchpad")]
    ConnectionError,
    #[error("error sending to the Launchpad")]
    SendError(#[from] midir::SendError),
}

impl From<ConnectError<MidiOutput>> for ConnectionError {
    fn from(_: ConnectError<MidiOutput>) -> Self {
        ConnectionError::ConnectionError
    }
}

impl From<ConnectError<MidiInput>> for ConnectionError {
    fn from(_: ConnectError<MidiInput>) -> Self {
        ConnectionError::ConnectionError
    }
}

pub type X = u8;
pub type Y = u8;
pub fn coords_to_key(x: X, y: Y) -> Key {
    10 * y + x
}
pub fn key_to_coords(key: Key) -> (X, Y) {
    (key % 10, key / 10)
}

pub fn rect(a: Key, b: Key) -> impl Iterator<Item = Key> {
    let (x0, y0) = key_to_coords(a);
    let (x1, y1) = key_to_coords(b);
    assert!(x0 <= x1);
    assert!(y0 <= y1);
    iter::successors(Some(key_to_coords(a)), move |(x, y)| {
        if *x == x1 {
            if *y == y1 {
                None
            } else {
                Some((x0, y + 1))
            }
        } else {
            Some((x + 1, *y))
        }
    })
    .map(|(x, y)| coords_to_key(x, y))
}

pub fn line(start: Key, direction: Direction, length: u8) -> impl Iterator<Item = Key> {
    let (x0, y0) = key_to_coords(start);

    let (dx, dy): (i8, i8) = match direction {
        Direction::Up => (0, 1),
        Direction::Right => (1, 0 ),
        Direction::Down => (0, -1),
        Direction::Left => (-1, 0) ,
    };

    iter::successors(if length > 0 { Some((1, x0, y0)) } else { None }, move |(i, x, y)| {
        if *i < length {
            Some((i + 1, x.checked_add_signed(dx)?, y.checked_add_signed(dy)?))
        } else {
            None
        }
    })
    .map(|(_, x, y)| coords_to_key(x, y))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Up,
    Right,
    Down,
    Left,
}

pub type Key = u8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Color {
    Simple(SimpleColor),
    Complex(ComplexColor),
}

impl Color {
    pub const fn simple(n: u8) -> Color {
        Color::Simple(SimpleColor::Static(n))
    }
    pub const fn flashing(a: u8, b: u8) -> Color {
        Color::Complex(ComplexColor::Flashing(a, b))
    }
    pub const fn pulsing(n: u8) -> Color {
        Color::Simple(SimpleColor::Pulsing(n))
    }
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color::Complex(ComplexColor::Rgb(r, g, b))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SimpleColor {
    Static(u8),
    Flashing(u8),
    Pulsing(u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComplexColor {
    Static(u8),
    Flashing(u8, u8),
    Pulsing(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TextColor {
    Palette(u8),
    Rgb(u8, u8, u8),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Layout {
    Session,
    Drums,
    Keys,
    User,
    //DawFaders,
    Programmer,
}

impl From<&Layout> for u8 {
    fn from(layout: &Layout) -> Self {
        match layout {
            Layout::Session => 0,
            Layout::Drums => 4,
            Layout::Keys => 5,
            Layout::User => 6,
            //Layout::DawFaders => 0x0d,
            Layout::Programmer => 0x7f,
        }
    }
}

#[derive(Debug)]
pub enum Command<'a> {
    GetVersions,
    SetLayout(Layout),
    GetLayout,
    SetProgrammerMode(bool),
    GetProgrammerMode,
    KeyOn(Key, SimpleColor),
    KeyOff(Key),
    SetColors(&'a [(Key, ComplexColor)]),
    ScrollText {
        loops: Option<bool>,
        speed: Option<u8>,
        color: Option<TextColor>,
        text: Option<&'a str>,
    },
    SetAwake(bool),
    GetAwake,
    SetBrightness(u8),
    GetBrightness,
    SetLedFeedback(bool, bool),
    GetLedFeedback,
}

impl<'a> Command<'a> {
    fn append_to_vec(&self, buf: &mut Vec<u8>) -> io::Result<()> {
        match self {
            Command::GetVersions => buf.write_all(&[0xf0, 0x7e, 0x7f, 0x06, 0x01, 0xf7]),
            Command::SetLayout(layout) => buf.write_all(&[
                0xf0,
                0x00,
                0x20,
                0x29,
                0x02,
                0x0d,
                0x00,
                layout.into(),
                0xf7,
            ]),
            Command::GetLayout => buf.write_all(&[0xf0, 0x00, 0x20, 0x29, 0x02, 0x0d, 0x00, 0xf7]),
            Command::SetProgrammerMode(enabled) => buf.write_all(&[
                0xf0,
                0x00,
                0x20,
                0x29,
                0x02,
                0x0d,
                0x0e,
                (*enabled).into(),
                0xf7,
            ]),
            Command::GetProgrammerMode => {
                buf.write_all(&[0xf0, 0x00, 0x20, 0x29, 0x02, 0x0d, 0x0e, 0xf7])
            }
            Command::KeyOn(key, color) => {
                assert!(*key >= 11);
                assert!(*key <= 99);
                assert_ne!(*key % 10, 0);
                match color {
                    SimpleColor::Static(c) => buf.write_all(&[0x90, *key, *c]),
                    SimpleColor::Flashing(c) => buf.write_all(&[0x91, *key, *c]),
                    SimpleColor::Pulsing(c) => buf.write_all(&[0x92, *key, *c]),
                }
            }
            Command::KeyOff(key) => {
                assert!(*key >= 11);
                assert!(*key <= 99);
                assert_ne!(*key % 10, 0);
                buf.write_all(&[0x90, *key, 0])
            }
            Command::SetColors(colors) => {
                assert!(colors.len() <= 81);
                buf.write_all(&[0xf0, 0x00, 0x20, 0x29, 0x02, 0x0d, 0x03])?;
                for (key, color) in colors.iter() {
                    assert!(*key >= 11);
                    assert!(*key <= 99);
                    assert_ne!(*key % 10, 0);
                    match color {
                        ComplexColor::Static(c) => buf.write_all(&[0, *key, *c])?,
                        ComplexColor::Flashing(b, a) => buf.write_all(&[1, *key, *b, *a])?,
                        ComplexColor::Pulsing(c) => buf.write_all(&[2, *key, *c])?,
                        ComplexColor::Rgb(r, g, b) => buf.write_all(&[3, *key, *r, *g, *b])?,
                    }
                }
                buf.write_all(&[0xf7])?;
                Ok(())
            }
            Command::ScrollText {
                loops,
                speed,
                color,
                text,
            } => {
                buf.write_all(&[0xf0, 0x00, 0x20, 0x29, 0x02, 0x0d, 0x07])?;
                if let Some(l) = loops {
                    buf.write_all(&[(*l).into()])?;
                }
                if let Some(s) = speed {
                    assert!(loops.is_some());
                    buf.write_all(&[*s])?;
                }
                if let Some(c) = color {
                    assert!(speed.is_some());
                    match c {
                        TextColor::Palette(p) => buf.write_all(&[0x00, *p])?,
                        TextColor::Rgb(r, g, b) => buf.write_all(&[0x01, *r, *g, *b])?,
                    };
                }
                if let Some(t) = text {
                    assert!(color.is_some());
                    buf.write_all(t.as_ref())?;
                }
                buf.write_all(&[0xf7])?;
                Ok(())
            }
            #[rustfmt::skip]
            Command::SetAwake(awake) => buf.write_all(&[0xf0, 0x00, 0x20, 0x29, 0x02, 0x0d, 0x09, (*awake).into(), 0xf7]),
            #[rustfmt::skip]
            Command::GetAwake => buf.write_all(&[0xf0, 0x00, 0x20, 0x29, 0x02, 0x0d, 0x09, 0xf7]),
            #[rustfmt::skip]
            Command::SetBrightness(brightness) => buf.write_all(&[0xf0, 0x00, 0x20, 0x29, 0x02, 0x0d, 0x08, *brightness, 0xf7]),
            #[rustfmt::skip]
            Command::GetBrightness => buf.write_all(&[0xf0, 0x00, 0x20, 0x29, 0x02, 0x0d, 0x08, 0xf7]),
            #[rustfmt::skip]
            Command::SetLedFeedback(internal, external) => buf.write_all(&[0xf0, 0x00, 0x20, 0x29, 0x02, 0x0d, 0x0a, (*internal).into(), (*external).into(), 0xf7]),
            #[rustfmt::skip]
            Command::GetLedFeedback => buf.write_all(&[0xf0, 0x00, 0x20, 0x29, 0x02, 0x0d, 0x0a, 0xf7]),
        }
    }
}

#[derive(Debug)]
pub enum Message {
    KeyDown(Key),
    KeyUp(Key),
    ApplicationVersion([u8; 4]),
    BootloaderVersion([u8; 4]),
    Layout(u8),
    ProgrammerMode(bool),
    Awake(bool),
    Brightness(u8),
    LedFeedback(bool, bool),
}

impl From<&[u8]> for Message {
    fn from(message: &[u8]) -> Self {
        use Message::*;
        match *message {
            // accept either Note On or Control Change (the former for the 8x8 grid, the latter for
            // the buttons at the top/side)
            [0x90 | 0xb0, note, 127] => KeyDown(note),
            [0x90 | 0xb0, note, 0] => KeyUp(note),
            [0xf0, 0x7e, 0x00, 0x06, 0x02, 0x00, 0x20, 0x29, 0x13, 0x01, 0x00, 0x00, a, b, c, d, 0xf7] => {
                ApplicationVersion([a, b, c, d])
            }
            [0xf0, 0x7e, 0x00, 0x06, 0x02, 0x00, 0x20, 0x29, 0x13, 0x11, 0x00, 0x00, a, b, c, d, 0xf7] => {
                BootloaderVersion([a, b, c, d])
            }
            [0xf0, 0x00, 0x20, 0x29, 0x02, 0x0d, 0x00, layout, 0xf7] => Layout(layout),
            [0xf0, 0x00, 0x20, 0x29, 0x02, 0x0d, 0x0e, mode, 0xf7] => ProgrammerMode(mode == 1),
            [0xf0, 0x00, 0x20, 0x29, 0x02, 0x0d, 0x09, awake, 0xf7] => Awake(awake == 1),
            [0xf0, 0x00, 0x20, 0x29, 0x02, 0x0d, 0x08, brightness, 0xf7] => Brightness(brightness),
            [0xf0, 0x00, 0x20, 0x29, 0x02, 0x0d, 0x0a, internal, external, 0xf7] => {
                LedFeedback(internal == 1, external == 1)
            }
            _ => unimplemented!(),
        }
    }
}

impl Launchpad {
    pub fn connect<T: FnMut(u64, Message) + Send + 'static>(
        mut callback: T,
    ) -> Result<Launchpad, ConnectionError> {
        let midi_in = MidiInput::new("midir input")?;
        let midi_out = MidiOutput::new("midir output")?;

        let midi_out_port = midi_out
            .ports()
            .into_iter()
            .find(|p| midi_out.port_name(p).unwrap().contains("LPMiniMK3 MI"))
            .ok_or(ConnectionError::NotFoundError)?;
        let out_con = midi_out.connect(&midi_out_port, "to launchpad")?;

        let midi_in_port = midi_in
            .ports()
            .into_iter()
            .find(|p| midi_in.port_name(p).unwrap().contains("LPMiniMK3 MI"))
            .expect("no launchpad found");
        let in_con = midi_in.connect(
            &midi_in_port,
            "from launchpad",
            move |ts, data, _| callback(ts, data.into()),
            (),
        )?;
        let mut launchpad = Launchpad {
            out_con,
            _in_con: in_con,
            send_buf: Vec::with_capacity(10),
            complex_color_buf: Vec::with_capacity(81),
            // current: [Color::Simple(SimpleColor::Static(0)); 100],
            current: HashMap::with_capacity(81),
        };
        for key in rect(11, 99) {
            launchpad
                .current
                .insert(key, Color::Simple(SimpleColor::Static(0)));
        }
        // switch to programmer mode
        launchpad.send(&Command::SetProgrammerMode(true))?;
        Ok(launchpad)
    }

    fn _send(
        command: &Command,
        send_buf: &mut Vec<u8>,
        out_con: &mut midir::MidiOutputConnection,
    ) -> Result<(), ConnectionError> {
        send_buf.clear();
        command.append_to_vec(send_buf).unwrap();
        out_con.send(send_buf)?;
        Ok(())
    }

    pub fn send(&mut self, command: &Command) -> Result<(), ConnectionError> {
        Launchpad::_send(command, &mut self.send_buf, &mut self.out_con)?;
        if let Command::KeyOn(key, color) = command {
            // self.current[*key as usize] = Color::Simple(*color);
            *self.current.get_mut(key).unwrap() = Color::Simple(*color);
        } else if let Command::SetColors(colors) = command {
            for (key, color) in colors.iter() {
                // self.current[*key as usize] = Color::Complex(*color);
                *self.current.get_mut(key).unwrap() = Color::Complex(*color);
            }
        }
        Ok(())
    }

    pub fn full_update(&mut self, new: &HashMap<Key, Color>) -> Result<(), ConnectionError> {
        self.complex_color_buf.clear();
        for key in rect(11, 99) {
            if new[&key] != self.current[&key] {
                *self.current.get_mut(&key).unwrap() = new[&key];
                match new[&key] {
                    Color::Simple(c) => Launchpad::_send(
                        &Command::KeyOn(key, c),
                        &mut self.send_buf,
                        &mut self.out_con,
                    )?,
                    Color::Complex(c) => self.complex_color_buf.push((key, c)),
                }
            }
        }
        if !self.complex_color_buf.is_empty() {
            Launchpad::_send(
                &Command::SetColors(&self.complex_color_buf),
                &mut self.send_buf,
                &mut self.out_con,
            )?;
        }
        Ok(())
    }
}

impl Drop for Launchpad {
    fn drop(&mut self) {
        if let Err(e) = self.send(&Command::SetProgrammerMode(false)) {
            eprintln!("warning: could not deinitialise Launchpad: {}", e);
        }
        // we would *like* to be able to do `self.{_in,out}_con.close()` here, but since they consume
        // the connection objects and we can't consume the `Launchpad` object here, we can't.
        // ...hopefully that won't cause anything bad to happen?
    }
}
