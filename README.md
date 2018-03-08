# Simple restreamer

Based on [tokio](tokio.rs) `chat.rs` example.

## Usage

Launch the application with `cargo run`, connect a producer to `localhost:12345`, connect any number of consumers to `localhost:12346`.
The application has cli options to override the ports (`-p`) and the host address (`-P` and `-C`).

