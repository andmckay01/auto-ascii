//! Play a `.slpy` asset in this terminal. The whole engine in one call:
//! capability probe, letterbox, live resize, terminal restore on quit,
//! Ctrl-C or panic. Press `q`/`Esc` to quit, `0`–`9` to seek.
//!
//! Make an asset:  `sleepy-factory build clip.mp4 -o intro.slpy`
//! Then:           `cargo run --example simple-play -- intro.slpy`

use sleepytime::Player;

fn main() -> Result<(), sleepytime::Error> {
    let asset = std::env::args().nth(1).expect("usage: simple-play <asset.slpy>");
    Player::builder().asset(asset).looping(true).build()?.run()
}
