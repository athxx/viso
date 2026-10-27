//! `print!`, `println!`, `eprint!` and `eprintln!` for every target: natively
//! what `std`'s print, and on the web, where `std` drops what is printed, each
//! line goes to the page's console (output to `console.log`, errors to
//! `console.error`), where devtools and the dev session read it. A glob
//! import cannot shadow `std`'s, so they are named by path (`viso::println!`)
//! or imported by name (`use viso::println;`).

#[doc(hidden)]
#[macro_export]
macro_rules! __print {
    ($($arg:tt)*) => {
        $crate::__private::viso_platform::stdio::write(
            $crate::__private::viso_platform::stdio::Stream::Out,
            ::core::format_args!($($arg)*),
        )
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __println {
    () => {
        $crate::__print!("\n")
    };
    ($($arg:tt)*) => {
        $crate::__private::viso_platform::stdio::write(
            $crate::__private::viso_platform::stdio::Stream::Out,
            ::core::format_args!("{}\n", ::core::format_args!($($arg)*)),
        )
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __eprint {
    ($($arg:tt)*) => {
        $crate::__private::viso_platform::stdio::write(
            $crate::__private::viso_platform::stdio::Stream::Err,
            ::core::format_args!($($arg)*),
        )
    };
}

#[doc(hidden)]
#[macro_export]
macro_rules! __eprintln {
    () => {
        $crate::__eprint!("\n")
    };
    ($($arg:tt)*) => {
        $crate::__private::viso_platform::stdio::write(
            $crate::__private::viso_platform::stdio::Stream::Err,
            ::core::format_args!("{}\n", ::core::format_args!($($arg)*)),
        )
    };
}
