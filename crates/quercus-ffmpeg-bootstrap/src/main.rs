fn main() {
	let prefix = quercus_ffmpeg_bootstrap::ensure_prefix();
	println!("{}", prefix.display());
}
