//! Print the blocks of one chapter: cargo run --example dump -- BOOK.epub CHAPTER
use rusty_reader::book::Book;

fn main() {
    let mut args = std::env::args().skip(1);
    let book = Book::open(args.next().expect("book").as_ref()).expect("open");
    let ch: usize = args.next().map_or(0, |s| s.parse().unwrap());
    println!(
        "{} chapters; images: {:?}",
        book.chapters.len(),
        book.images.keys().take(5).collect::<Vec<_>>()
    );
    for b in book.chapters[ch].blocks.iter().take(20) {
        println!("{:?} {:?}", b.kind, b.plain_text().chars().take(60).collect::<String>());
    }
}
