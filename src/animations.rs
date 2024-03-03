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
                .map(Some),
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
            .map(Some),
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
                    let mut x;
                    let mut y;
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
pub(crate) fn alert(launchpad: &mut Launchpad, focus: Option<u8>) -> eyre::Result<()> {
    let is_real_focus = focus.is_some();
    let focus = focus.unwrap_or(55);
    let (focus_x, focus_y) = key_to_coords(focus);
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

    // phase 2: wait a bit
    thread::sleep(Duration::from_millis(900));

    // phase 3: contract back towards the focus
    let mut up = up_from(focus).skip(1);
    let mut down = down_from(focus).skip(1);
    let mut left = left_from(focus).skip(1);
    let mut right = right_from(focus).skip(1);
    let mut bounds = Vec::new();
    loop {
        match (up.next(), down.next(), left.next(), right.next()) {
            (None, None, None, None) => break,
            (u, d, l, r) => {
                bounds.push((u, d, l, r));
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

    // clean up the pulsing focus (unless it's wanted by the caller)
    if !is_real_focus {
        launchpad.send(&Command::KeyOff(coords_to_key(focus_x, focus_y)))?;
    }

    Ok(())
}
