use std::env;
use std::error::Error;
use std::fs;

fn main() -> Result<(), Box<dyn Error>> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("usage: fl <script-file>");
        std::process::exit(1);
    }
    let src = fs::read_to_string(&args[1])?;
    let out = firelang::run_script(&src)?;
    println!("=> {out:?}");
    Ok(())
}
