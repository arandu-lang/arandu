//! Where command output goes. A batch command writes to the process streams.
//! A daemon build (HOT.3) installs a sink on its own thread and forwards each
//! piece of text to the client that asked for it, with the client's color and
//! width for rendered diagnostics.

use std::cell::{Cell, RefCell};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// How diagnostics rendered on this thread look: the client's terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderStyle {
    pub color: bool,
    pub width: Option<usize>,
}

type Sink = Box<dyn FnMut(Stream, &str)>;

thread_local! {
    static SINK: RefCell<Option<Sink>> = RefCell::new(None);
    static STYLE: Cell<Option<RenderStyle>> = const { Cell::new(None) };
}

/// Writes `text` to the sink of this thread, or to the process stream.
pub fn write(stream: Stream, text: &str) {
    let forwarded = SINK.with(|sink| match sink.borrow_mut().as_mut() {
        Some(sink) => {
            sink(stream, text);
            true
        }
        None => false,
    });
    if !forwarded {
        match stream {
            Stream::Stdout => print!("{text}"),
            Stream::Stderr => eprint!("{text}"),
        }
    }
}

/// The style the client of this thread asked for, if any.
pub fn style() -> Option<RenderStyle> {
    STYLE.with(Cell::get)
}

/// Runs `body` with every write of this thread going to `sink` and diagnostics
/// rendered in `style`. Both are removed afterwards, also on unwinding.
pub fn capture<R>(
    sink: impl FnMut(Stream, &str) + 'static,
    style: RenderStyle,
    body: impl FnOnce() -> R,
) -> R {
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            SINK.with(|sink| sink.borrow_mut().take());
            STYLE.with(|current| current.set(None));
        }
    }
    SINK.with(|current| *current.borrow_mut() = Some(Box::new(sink)));
    STYLE.with(|current| current.set(Some(style)));
    let _restore = Restore;
    body()
}

/// `println!` through [`write`].
macro_rules! outln {
    ($($arg:tt)*) => {
        $crate::output::write(
            $crate::output::Stream::Stdout,
            &format!("{}\n", format_args!($($arg)*)),
        )
    };
}

/// `eprintln!` through [`write`].
macro_rules! errln {
    ($($arg:tt)*) => {
        $crate::output::write(
            $crate::output::Stream::Stderr,
            &format!("{}\n", format_args!($($arg)*)),
        )
    };
}

/// `eprint!` through [`write`].
macro_rules! errp {
    ($($arg:tt)*) => {
        $crate::output::write($crate::output::Stream::Stderr, &format!($($arg)*))
    };
}

pub(crate) use {errln, errp, outln};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_capture_receives_this_thread_s_writes_and_ends_with_its_body() {
        let captured = std::rc::Rc::new(RefCell::new(Vec::new()));
        let sink = std::rc::Rc::clone(&captured);
        let style = RenderStyle {
            color: true,
            width: Some(100),
        };
        capture(
            move |stream, text| sink.borrow_mut().push((stream, text.to_owned())),
            style,
            || {
                outln!("built {}", 1);
                errp!("warn");
                assert_eq!(super::style(), Some(style));
            },
        );
        assert_eq!(super::style(), None);
        assert_eq!(
            *captured.borrow(),
            [
                (Stream::Stdout, "built 1\n".to_owned()),
                (Stream::Stderr, "warn".to_owned())
            ]
        );
    }
}
