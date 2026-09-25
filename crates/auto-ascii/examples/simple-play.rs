//! Play one asset in the terminal with the public Player builder.
//! Pass an asset path; playback loops until the viewer quits.

use auto_ascii::Player;

fn main() -> Result<(), auto_ascii::Error> {
    let asset = std::env::args().nth(1).expect("usage: simple-play <asset.ascii>");
    Player::builder().asset(asset).looping(true).build()?.run()
}
