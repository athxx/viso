//! `viso::print!` and its family print as `std`'s do on every target, and to
//! the page's console on the web, where `std` drops what is printed.

use viso::{eprint, eprintln, print, println};

#[test]
fn imported_by_name_they_print_as_std_does() {
    let n = 3;
    print!("{n} ");
    println!("{n:>4}");
    println!();
    eprint!("{}", "err ");
    eprintln!("{n:02}");
    eprintln!();
    viso::println!("by path");
}
