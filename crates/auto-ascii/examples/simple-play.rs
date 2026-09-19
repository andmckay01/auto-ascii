//! Play a `.ascii` asset in this terminal. The whole engine in one call:
//! capability probe, letterbox, live resize, terminal restore on quit,
//! Ctrl-C or panic. Press `q`/`Esc` to quit, `0`–`9` to seek.
//!
//! Make an asset:  `auto-ascii-factory build clip.mp4 -o intro.ascii`
//! Then:           `cargo run --example simple-play -- intro.ascii`

use auto_ascii::Player;

fn main() -> Result<(), auto_ascii::Error> {
    let asset = std::env::args().nth(1).expect("usage: simple-play <asset.ascii>");
    Player::builder().asset(asset).looping(true).build()?.run()
}
