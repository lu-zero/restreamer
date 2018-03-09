# Simple restreamer

[![LICENSE](https://img.shields.io/badge/license-GPL-blue.svg)](COPYING)

Based on [tokio](tokio.rs) `chat.rs` example.

## Usage

Launch the application with `cargo run`, connect a producer to `localhost:12345`, connect any number of consumers to `localhost:12346`.

The application has cli options to override the ports (`-p`), the host addresses (`-I` and `-O`) and the internal buffer size `-b`.

```
restream 0.1.0
Luca Barbato <lu_zero@gentoo.org>

USAGE:
    restream [OPTIONS]

FLAGS:
    -h, --help       Prints help information
    -V, --version    Prints version information

OPTIONS:
    -b <buffer>             Set the packet buffer size [default: 1316]
    -I <input_host>         Set the input host [default: 127.0.0.1]
    -O <output_host>        Set the output host [default: 127.0.0.1]
    -p, --port <port>       Set listening ports [default: 12345]
```

## Credits

Thanks to [TodoStreaming](http://www.todostreaming.es) for sponsoring this experiment.
