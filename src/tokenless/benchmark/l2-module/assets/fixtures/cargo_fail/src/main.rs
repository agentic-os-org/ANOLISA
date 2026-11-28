//! L2 fixture: a build that fails with one type error.

fn main() {
    let ready: bool = dep01::value() + dep02::value();
    let _ = (ready, dep03::value(), dep04::value());
}
