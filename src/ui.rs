use crate::Event;
use lp::{Color, Command, Key, Launchpad, SimpleColor, TextColor};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::HashMap;
use std::panic::Location;
use std::process;
use std::sync::mpsc;

pub(crate) struct Ui<'a> {
    pub(crate) fb: &'a mut HashMap<Key, Color>,
    pub(crate) event: Event,
    pub(crate) launchpad_for_side_effects: &'a mut Launchpad,
    pub(crate) tx_for_side_effects: &'a mpsc::Sender<Event>,
}
impl<'a> Ui<'a> {
    /// A tabstrip widget.
    #[track_caller]
    pub(crate) fn tabs<const LEN: u8>(&mut self, start: Key) -> u8 {
        static DATA: Lazy<Mutex<HashMap<&Location, u8>>> = Lazy::new(|| {
            let m = HashMap::with_capacity(1);
            Mutex::new(m)
        });
        let mut data = DATA.lock();
        let tab = data.entry(Location::caller()).or_insert(0);
        *tab = match self.event {
            Event::KeyDown(key) if key >= start && key < start + LEN => key - start,
            _ => *tab,
        };
        for (i, k) in (start..start + LEN).enumerate() {
            *self.fb.get_mut(&k).unwrap() = if *tab == i as u8 {
                Color::Simple(SimpleColor::Static(20))
            } else {
                Color::Simple(SimpleColor::Static(1))
            };
        }
        *tab
    }
    /// A static, unchanging colour.
    #[track_caller]
    pub(crate) fn static_color(&mut self, key: Key, color: Color) {
        *self.fb.get_mut(&key).unwrap() = color;
    }
    /// A toggleable button.
    #[track_caller]
    pub(crate) fn toggle_button(&mut self, key: Key, inactive_color: Color, active_color: Color) -> bool {
        static DATA: Lazy<Mutex<HashMap<(Key, &Location), bool>>> =
            Lazy::new(|| Mutex::new(HashMap::new()));
        let mut data = DATA.lock();
        let enabled = data.entry((key, Location::caller())).or_insert(false);
        *enabled = match self.event {
            Event::KeyDown(k) if k == key => !*enabled,
            _ => *enabled,
        };
        *self.fb.get_mut(&key).unwrap() = if *enabled {
            active_color
        } else {
            inactive_color
        };
        *enabled
    }
    /// A pair of buttons that decrement and increment a counter respectively.
    #[track_caller]
    pub(crate) fn counter_buttons<const MAX: i64>(&mut self, start: Key) -> i64 {
        static DATA: Lazy<Mutex<HashMap<(Key, &Location), i64>>> =
            Lazy::new(|| Mutex::new(HashMap::new()));
        let mut data = DATA.lock();
        let n = data.entry((start, Location::caller())).or_insert(0);
        *n += match self.event {
            Event::KeyDown(k) if k == start => -1,
            Event::KeyDown(k) if k == start + 1 => 1,
            _ => 0,
        };
        if *n == MAX {
            *n = 0;
        } else if *n == -1 {
            *n = MAX - 1;
        }
        *self.fb.get_mut(&start).unwrap() = match self.event {
            Event::KeyDown(k) if k == start => Color::Simple(SimpleColor::Static(2)),
            _ => Color::Simple(SimpleColor::Static(1)),
        };
        *self.fb.get_mut(&(start + 1)).unwrap() = match self.event {
            Event::KeyDown(k) if k == start + 1 => Color::Simple(SimpleColor::Static(2)),
            _ => Color::Simple(SimpleColor::Static(1)),
        };
        *n
    }
    /// A button that displays text when pressed.
    #[track_caller]
    pub(crate) fn info_button(&mut self, key: Key, color: Color, text: &str) {
        *self.fb.get_mut(&key).unwrap() = color;
        if let Event::KeyDown(k) = self.event {
            if k == key {
                self.launchpad_for_side_effects
                    .send(&Command::ScrollText {
                        loops: Some(false),
                        speed: Some(15),
                        color: Some(TextColor::Palette(3)),
                        text: Some(text),
                    })
                    .unwrap()
            }
        }
    }
    /// A button that returns true once when pressed.
    #[track_caller]
    pub(crate) fn impulse_button(&mut self, key: Key, color: Color, pressed_color: Color) -> bool {
        static DATA: Lazy<Mutex<HashMap<(Key, &Location), bool>>> =
            Lazy::new(|| Mutex::new(HashMap::new()));
        let mut data = DATA.lock();
        let pressed = data.entry((key, Location::caller())).or_insert(false);
        *pressed = match self.event {
            Event::KeyDown(k) if k == key => true,
            Event::KeyUp(k) if k == key => false,
            _ => *pressed,
        };
        *self.fb.get_mut(&key).unwrap() = if *pressed { pressed_color } else { color };
        if let Event::KeyDown(k) = self.event {
            k == key
        } else {
            false
        }
    }
    /// A button that can be pressed, released, or that nothing can happen to.
    #[track_caller]
    pub(crate) fn press_release_button(
        &mut self,
        key: Key,
        color: Color,
        pressed_color: Color,
    ) -> Option<bool> {
        static DATA: Lazy<Mutex<HashMap<(Key, &Location), bool>>> =
            Lazy::new(|| Mutex::new(HashMap::new()));
        let mut data = DATA.lock();
        let pressed = data.entry((key, Location::caller())).or_insert(false);
        *pressed = match self.event {
            Event::KeyDown(k) if k == key => true,
            Event::KeyUp(k) if k == key => false,
            _ => *pressed,
        };
        *self.fb.get_mut(&key).unwrap() = if *pressed { pressed_color } else { color };
        match self.event {
            Event::KeyDown(k) if k == key => Some(true),
            Event::KeyUp(k) if k == key => Some(false),
            _ => None,
        }
    }
    /// A helper function that returns `true` exactly once each time `val` becomes `true`.
    #[track_caller]
    pub(crate) fn monostable(&mut self, val: bool, extra_key: u8) -> bool {
        static DATA: Lazy<Mutex<HashMap<(u8, &Location), bool>>> =
            Lazy::new(|| Mutex::new(HashMap::new()));
        let mut data = DATA.lock();
        let prev = data.entry((extra_key, Location::caller())).or_insert(val);
        let ret = val && !*prev;
        *prev = val;
        ret
    }
    /// A button that returns whether it is currently held down.
    #[track_caller]
    pub(crate) fn holdable_button(&mut self, key: Key, color: Color, pressed_color: Color) -> bool {
        static DATA: Lazy<Mutex<HashMap<(Key, &Location), bool>>> =
            Lazy::new(|| Mutex::new(HashMap::new()));
        let mut data = DATA.lock();
        let pressed = data.entry((key, Location::caller())).or_insert(false);
        *pressed = match self.event {
            Event::KeyDown(k) if k == key => true,
            Event::KeyUp(k) if k == key => false,
            _ => *pressed,
        };
        *self.fb.get_mut(&key).unwrap() = if *pressed { pressed_color } else { color };
        *pressed
    }
    /// A slider to control LED brightness.
    #[track_caller]
    pub(crate) fn led_slider(&mut self, start: Key) {
        assert_eq!(start % 10, 1);
        static DATA: Lazy<Mutex<Option<u8>>> = Lazy::new(|| Mutex::new(None));
        let mut brightness = DATA.lock();
        if let Event::Brightness(b) = self.event {
            *brightness = Some(b);
        }
        if brightness.is_none() {
            // thread::sleep(Duration::from_millis(2)); // XXX HACK EW EW EW
            self.launchpad_for_side_effects
                .send(&Command::GetBrightness)
                .unwrap();
        }
        for i in 0..8 {
            let color = if brightness.unwrap_or(255) / 16 == i {
                Color::Simple(SimpleColor::Static(113))
            } else {
                Color::Simple(SimpleColor::Static(104))
            };
            if self.impulse_button(start + i, color, color) {
                // the "correct" sequence here is: 0, 18, 36, 54, 72, 91, 109, 127
                // integer maths gives 90 and 108, not 91 and 109:
                //     (i as u64 * 127 / 7) as u8
                // floating-point maths and rounding gives 73, not 72:
                //     ((i as f32 * 127. / 7.).round()) as u8
                // so we have to bias the result a little by subtracting 0.1 after the division
                // hey novation: ????????
                let b = (i as f32 * 127. / 7. - 0.1).round() as u8;
                self.launchpad_for_side_effects
                    .send(&Command::SetBrightness(b))
                    .unwrap();
                self.launchpad_for_side_effects
                    .send(&Command::GetBrightness)
                    .unwrap();
            }
        }
    }
    /// A button that quits the application when pressed.
    #[track_caller]
    pub(crate) fn exit_button(&mut self, key: Key) {
        if self.impulse_button(
            key,
            Color::Simple(SimpleColor::Static(6)),
            Color::Simple(SimpleColor::Static(6)),
        ) {
            // delayed by an iteration of the loop... not ideal, but quick and easy
            self.tx_for_side_effects.send(Event::Exit).unwrap();
        }
    }
    /// A sleep button. Designed to be wrapped around the entire UI; when asleep, reacts to and rewrites any button-press to a plain redraw.
    #[track_caller]
    pub(crate) fn awake(&mut self, key: Key, color: Color) -> bool {
        static DATA: Lazy<Mutex<HashMap<(Key, &Location), bool>>> =
            Lazy::new(|| Mutex::new(HashMap::new()));
        let mut data = DATA.lock();
        let awake = data.entry((key, Location::caller())).or_insert(true);
        *awake = match (*awake, &self.event) {
            (true, &Event::KeyDown(k)) if k == key => false,
            (true, _) => *awake,
            (false, &Event::KeyUp(k)) if k == key => *awake,
            (false, &Event::KeyDown(_)) => {
                self.event = Event::Redraw;
                true
            }
            (false, _) => *awake,
        };
        *self.fb.get_mut(&key).unwrap() = if *awake {
            color
        } else {
            Color::Simple(SimpleColor::Static(0))
        };
        *awake
    }
    #[track_caller]
    pub(crate) fn play_pause_button(
        &mut self,
        key: Key,
        playing_color: Color,
        paused_color: Color,
    ) -> eyre::Result<()> {
        static DATA: Lazy<Mutex<bool>> = Lazy::new(|| {
            Mutex::new(
                duct::cmd!("playerctl", "status")
                    .unchecked()
                    .read()
                    .unwrap()
                    == "Playing",
            )
        });
        let mut data = DATA.lock();
        let playing = &mut *data;
        *playing = match self.event {
            Event::MediaPlaying(p) => p,
            _ => *playing,
        };
        let color = if *playing {
            playing_color
        } else {
            paused_color
        };
        if self.impulse_button(key, color, color) {
            if *playing {
                process::Command::new("playerctl").arg("pause").status()?;
            } else {
                process::Command::new("playerctl").arg("play").status()?;
            }
        }
        Ok(())
    }
}
