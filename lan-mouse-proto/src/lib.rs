use input_event::{Event as InputEvent, KeyboardEvent, PointerEvent};
use num_enum::{IntoPrimitive, TryFromPrimitive, TryFromPrimitiveError};
use paste::paste;
use std::{
    fmt::{Debug, Display, Formatter},
    mem::size_of,
};
use thiserror::Error;

/// defines the maximum size an encoded event can take up
/// this is currently the pointer motion event
/// type: u8, time: u32, dx: f64, dy: f64
pub const MAX_EVENT_SIZE: usize = size_of::<u8>() + size_of::<u32>() + 2 * size_of::<f64>();

/// error type for protocol violations
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// event type does not exist
    #[error("invalid event id: `{0}`")]
    InvalidEventId(#[from] TryFromPrimitiveError<EventType>),
    /// position type does not exist
    #[error("invalid event id: `{0}`")]
    InvalidPosition(#[from] TryFromPrimitiveError<Position>),
}

/// Position of a client
#[derive(Clone, Copy, Debug, TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum Position {
    Left,
    Right,
    Top,
    Bottom,
}

impl Display for Position {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let pos = match self {
            Position::Left => "left",
            Position::Right => "right",
            Position::Top => "top",
            Position::Bottom => "bottom",
        };
        write!(f, "{pos}")
    }
}

/// main lan-mouse protocol event type
#[derive(Clone, Copy, Debug)]
pub enum ProtoEvent {
    /// notify a client that the cursor entered its region at the given position
    /// [`ProtoEvent::Ack`] with the same serial is used for synchronization between devices
    /// the optional fraction (0..1) is the position along the edge the cursor entered at
    Enter(Position, Option<f64>),
    /// notify a client that the cursor left its region
    /// [`ProtoEvent::Ack`] with the same serial is used for synchronization between devices
    /// the optional fraction (0..1) is the position along the edge the cursor left at
    Leave(u32, Option<f64>),
    /// acknowledge of an [`ProtoEvent::Enter`] or [`ProtoEvent::Leave`] event
    Ack(u32),
    /// Input event
    Input(InputEvent),
    /// Ping event for tracking unresponsive clients.
    /// A client has to respond with [`ProtoEvent::Pong`].
    Ping,
    /// Response to [`ProtoEvent::Ping`], true if emulation is enabled / available
    Pong(bool),
    /// Build identification for the sending peer. Sent by the
    /// connect side once after the connection authenticates, and
    /// echoed back by the listen side in reply, so each end can
    /// display the peer's build hash and warn (soft) on mismatch.
    /// `commit` is the 8-byte ASCII short commit hash from
    /// `shadow_rs`'s `SHORT_COMMIT`. Old peers that don't
    /// recognize the event type silently skip it per the
    /// forward-compat handling in the receive loop.
    Hello { commit: [u8; 8] },
}

impl Display for ProtoEvent {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            ProtoEvent::Enter(s, None) => write!(f, "Enter({s})"),
            ProtoEvent::Enter(s, Some(e)) => write!(f, "Enter({s}, {e:.3})"),
            ProtoEvent::Leave(s, None) => write!(f, "Leave({s})"),
            ProtoEvent::Leave(s, Some(e)) => write!(f, "Leave({s}, {e:.3})"),
            ProtoEvent::Ack(s) => write!(f, "Ack({s})"),
            ProtoEvent::Input(e) => write!(f, "{e}"),
            ProtoEvent::Ping => write!(f, "ping"),
            ProtoEvent::Pong(alive) => {
                write!(
                    f,
                    "pong: {}",
                    if *alive { "alive" } else { "not available" }
                )
            }
            ProtoEvent::Hello { commit } => {
                let s = std::str::from_utf8(commit).unwrap_or("????????");
                write!(f, "Hello({s})")
            }
        }
    }
}

#[derive(TryFromPrimitive, IntoPrimitive)]
#[repr(u8)]
pub enum EventType {
    PointerMotion,
    PointerButton,
    PointerAxis,
    PointerAxisValue120,
    KeyboardKey,
    KeyboardModifiers,
    Ping,
    Pong,
    Enter,
    Leave,
    Ack,
    Hello,
}

impl ProtoEvent {
    fn event_type(&self) -> EventType {
        match self {
            ProtoEvent::Input(e) => match e {
                InputEvent::Pointer(p) => match p {
                    PointerEvent::Motion { .. } => EventType::PointerMotion,
                    PointerEvent::Button { .. } => EventType::PointerButton,
                    PointerEvent::Axis { .. } => EventType::PointerAxis,
                    PointerEvent::AxisDiscrete120 { .. } => EventType::PointerAxisValue120,
                },
                InputEvent::Keyboard(k) => match k {
                    KeyboardEvent::Key { .. } => EventType::KeyboardKey,
                    KeyboardEvent::Modifiers { .. } => EventType::KeyboardModifiers,
                },
            },
            ProtoEvent::Ping => EventType::Ping,
            ProtoEvent::Pong(_) => EventType::Pong,
            ProtoEvent::Enter(..) => EventType::Enter,
            ProtoEvent::Leave(..) => EventType::Leave,
            ProtoEvent::Ack(_) => EventType::Ack,
            ProtoEvent::Hello { .. } => EventType::Hello,
        }
    }
}

impl TryFrom<[u8; MAX_EVENT_SIZE]> for ProtoEvent {
    type Error = ProtocolError;

    fn try_from(buf: [u8; MAX_EVENT_SIZE]) -> Result<Self, Self::Error> {
        let mut buf = &buf[..];
        let event_type = decode_u8(&mut buf)?;
        match EventType::try_from(event_type)? {
            EventType::PointerMotion => {
                Ok(Self::Input(InputEvent::Pointer(PointerEvent::Motion {
                    time: decode_u32(&mut buf)?,
                    dx: decode_f64(&mut buf)?,
                    dy: decode_f64(&mut buf)?,
                })))
            }
            EventType::PointerButton => {
                Ok(Self::Input(InputEvent::Pointer(PointerEvent::Button {
                    time: decode_u32(&mut buf)?,
                    button: decode_u32(&mut buf)?,
                    state: decode_u32(&mut buf)?,
                })))
            }
            EventType::PointerAxis => Ok(Self::Input(InputEvent::Pointer(PointerEvent::Axis {
                time: decode_u32(&mut buf)?,
                axis: decode_u8(&mut buf)?,
                value: decode_f64(&mut buf)?,
            }))),
            EventType::PointerAxisValue120 => Ok(Self::Input(InputEvent::Pointer(
                PointerEvent::AxisDiscrete120 {
                    axis: decode_u8(&mut buf)?,
                    value: decode_i32(&mut buf)?,
                },
            ))),
            EventType::KeyboardKey => Ok(Self::Input(InputEvent::Keyboard(KeyboardEvent::Key {
                time: decode_u32(&mut buf)?,
                key: decode_u32(&mut buf)?,
                state: decode_u8(&mut buf)?,
            }))),
            EventType::KeyboardModifiers => Ok(Self::Input(InputEvent::Keyboard(
                KeyboardEvent::Modifiers {
                    depressed: decode_u32(&mut buf)?,
                    latched: decode_u32(&mut buf)?,
                    locked: decode_u32(&mut buf)?,
                    group: decode_u32(&mut buf)?,
                },
            ))),
            EventType::Ping => Ok(Self::Ping),
            EventType::Pong => Ok(Self::Pong(decode_u8(&mut buf)? != 0)),
            EventType::Enter => {
                let pos = decode_u8(&mut buf)?.try_into()?;
                Ok(Self::Enter(pos, decode_edge(&mut buf)?))
            }
            EventType::Leave => {
                let serial = decode_u32(&mut buf)?;
                Ok(Self::Leave(serial, decode_edge(&mut buf)?))
            }
            EventType::Ack => Ok(Self::Ack(decode_u32(&mut buf)?)),
            EventType::Hello => {
                let mut commit = [0u8; 8];
                for b in commit.iter_mut() {
                    *b = decode_u8(&mut buf)?;
                }
                Ok(Self::Hello { commit })
            }
        }
    }
}

impl From<ProtoEvent> for ([u8; MAX_EVENT_SIZE], usize) {
    fn from(event: ProtoEvent) -> Self {
        let mut buf = [0u8; MAX_EVENT_SIZE];
        let mut len = 0usize;
        {
            let mut buf = &mut buf[..];
            let buf = &mut buf;
            let len = &mut len;
            encode_u8(buf, len, event.event_type() as u8);
            match event {
                ProtoEvent::Input(event) => match event {
                    InputEvent::Pointer(p) => match p {
                        PointerEvent::Motion { time, dx, dy } => {
                            encode_u32(buf, len, time);
                            encode_f64(buf, len, dx);
                            encode_f64(buf, len, dy);
                        }
                        PointerEvent::Button {
                            time,
                            button,
                            state,
                        } => {
                            encode_u32(buf, len, time);
                            encode_u32(buf, len, button);
                            encode_u32(buf, len, state);
                        }
                        PointerEvent::Axis { time, axis, value } => {
                            encode_u32(buf, len, time);
                            encode_u8(buf, len, axis);
                            encode_f64(buf, len, value);
                        }
                        PointerEvent::AxisDiscrete120 { axis, value } => {
                            encode_u8(buf, len, axis);
                            encode_i32(buf, len, value);
                        }
                    },
                    InputEvent::Keyboard(k) => match k {
                        KeyboardEvent::Key { time, key, state } => {
                            encode_u32(buf, len, time);
                            encode_u32(buf, len, key);
                            encode_u8(buf, len, state);
                        }
                        KeyboardEvent::Modifiers {
                            depressed,
                            latched,
                            locked,
                            group,
                        } => {
                            encode_u32(buf, len, depressed);
                            encode_u32(buf, len, latched);
                            encode_u32(buf, len, locked);
                            encode_u32(buf, len, group);
                        }
                    },
                },
                ProtoEvent::Ping => {}
                ProtoEvent::Pong(alive) => encode_u8(buf, len, alive as u8),
                ProtoEvent::Enter(pos, edge) => {
                    encode_u8(buf, len, pos as u8);
                    encode_edge(buf, len, edge);
                }
                ProtoEvent::Leave(serial, edge) => {
                    encode_u32(buf, len, serial);
                    encode_edge(buf, len, edge);
                }
                ProtoEvent::Ack(serial) => encode_u32(buf, len, serial),
                ProtoEvent::Hello { commit } => {
                    for b in commit.iter() {
                        encode_u8(buf, len, *b);
                    }
                }
            }
        }
        (buf, len)
    }
}

macro_rules! decode_impl {
    ($t:ty) => {
        paste! {
            fn [<decode_ $t>](data: &mut &[u8]) -> Result<$t, ProtocolError> {
                let (int_bytes, rest) = data.split_at(size_of::<$t>());
                *data = rest;
                Ok($t::from_be_bytes(int_bytes.try_into().unwrap()))
            }
        }
    };
}

decode_impl!(u8);
decode_impl!(u16);
decode_impl!(u32);
decode_impl!(i32);
decode_impl!(f64);

macro_rules! encode_impl {
    ($t:ty) => {
        paste! {
            fn [<encode_ $t>](buf: &mut &mut [u8], amt: &mut usize, n: $t) {
                let src = n.to_be_bytes();
                let data = std::mem::take(buf);
                let (int_bytes, rest) = data.split_at_mut(size_of::<$t>());
                int_bytes.copy_from_slice(&src);
                *amt += size_of::<$t>();
                *buf = rest
            }
        }
    };
}

encode_impl!(u8);
encode_impl!(u16);
encode_impl!(u32);
encode_impl!(i32);
encode_impl!(f64);

/// Edge fractions are sent as u16: 0 means "no fraction" (this is also what
/// peers that do not send a fraction leave in the zeroed receive buffer),
/// otherwise `1 + round(fraction * 65534)`.
const EDGE_STEPS: f64 = 65534.0;

fn encode_edge(buf: &mut &mut [u8], amt: &mut usize, edge: Option<f64>) {
    let v = match edge {
        Some(f) if f.is_finite() => 1 + (f.clamp(0.0, 1.0) * EDGE_STEPS).round() as u16,
        _ => 0,
    };
    encode_u16(buf, amt, v);
}

fn decode_edge(data: &mut &[u8]) -> Result<Option<f64>, ProtocolError> {
    Ok(match decode_u16(data)? {
        0 => None,
        v => Some((v - 1) as f64 / EDGE_STEPS),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(event: ProtoEvent) -> ProtoEvent {
        let (buf, _len): ([u8; MAX_EVENT_SIZE], usize) = event.into();
        buf.try_into().unwrap()
    }

    #[test]
    fn enter_roundtrip() {
        for edge in [None, Some(0.0), Some(0.5), Some(1.0), Some(0.123)] {
            match roundtrip(ProtoEvent::Enter(Position::Top, edge)) {
                ProtoEvent::Enter(Position::Top, e) => match (edge, e) {
                    (None, None) => {}
                    (Some(a), Some(b)) => assert!((a - b).abs() < 1e-4),
                    _ => panic!("edge mismatch: {edge:?} != {e:?}"),
                },
                e => panic!("unexpected event {e}"),
            }
        }
    }

    #[test]
    fn leave_roundtrip() {
        for edge in [None, Some(0.0), Some(0.5), Some(1.0), Some(0.987)] {
            match roundtrip(ProtoEvent::Leave(42, edge)) {
                ProtoEvent::Leave(42, e) => match (edge, e) {
                    (None, None) => {}
                    (Some(a), Some(b)) => assert!((a - b).abs() < 1e-4),
                    _ => panic!("edge mismatch: {edge:?} != {e:?}"),
                },
                e => panic!("unexpected event {e}"),
            }
        }
    }

    #[test]
    fn edge_extremes_are_exact() {
        let ProtoEvent::Enter(_, Some(a)) = roundtrip(ProtoEvent::Enter(Position::Left, Some(0.0)))
        else {
            panic!()
        };
        let ProtoEvent::Enter(_, Some(b)) = roundtrip(ProtoEvent::Enter(Position::Left, Some(1.0)))
        else {
            panic!()
        };
        assert_eq!(a, 0.0);
        assert_eq!(b, 1.0);
    }

    #[test]
    fn old_peer_without_fraction() {
        // old peers send Enter as 2 bytes, the rest of the zeroed buffer is read as "no fraction"
        let mut buf = [0u8; MAX_EVENT_SIZE];
        buf[0] = EventType::Enter as u8;
        buf[1] = Position::Right as u8;
        assert!(matches!(
            buf.try_into().unwrap(),
            ProtoEvent::Enter(Position::Right, None)
        ));
        // old Leave: type + u32 serial
        let mut buf = [0u8; MAX_EVENT_SIZE];
        buf[0] = EventType::Leave as u8;
        assert!(matches!(
            buf.try_into().unwrap(),
            ProtoEvent::Leave(0, None)
        ));
    }
}
