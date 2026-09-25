use auto_ascii::Player;

fn main() -> Result<(), auto_ascii::Error> {
    let asset = std::env::args().nth(1).expect("usage: simple-play <asset.ascii>");
    Player::builder().asset(asset).looping(true).build()?.run()
}
