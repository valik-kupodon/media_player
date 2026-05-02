mod media_streams;

use media_streams::MediaStreams;

fn main() {
    let media_streams = MediaStreams::new("");

    if let Err(e) = media_streams.play() {
        eprintln!("Помилка при відтворенні медіа: {}", e);
    }
}
