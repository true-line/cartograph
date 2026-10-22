use mcp_local::{
    connection::{read_frame, write_frame},
    handshake::connect_authenticated,
    record::ConnectionRecord,
};
use std::{
    future::{Future, poll_fn},
    num::NonZeroUsize,
    path::PathBuf,
    pin::pin,
    task::Poll,
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};

struct Options {
    record: PathBuf,
    cap: NonZeroUsize,
    deadline: Duration,
}
impl Options {
    fn parse() -> Result<Self, &'static str> {
        let mut args = std::env::args_os().skip(1);
        let (mut record, mut cap, mut millis) = (None, None, None);
        while let Some(flag) = args.next() {
            let value = args.next().ok_or("invalid relay arguments")?;
            match flag.to_str() {
                Some("--record") if record.is_none() => record = Some(PathBuf::from(value)),
                Some("--max-frame-bytes") if cap.is_none() => {
                    cap = Some(
                        value
                            .to_str()
                            .and_then(|value| value.parse::<NonZeroUsize>().ok())
                            .ok_or("invalid relay arguments")?,
                    )
                }
                Some("--timeout-ms") if millis.is_none() => {
                    millis = Some(
                        value
                            .to_str()
                            .and_then(|value| value.parse::<std::num::NonZeroU64>().ok())
                            .ok_or("invalid relay arguments")?,
                    )
                }
                _ => return Err("invalid relay arguments"),
            }
        }
        let result = Self {
            record: record.ok_or("invalid relay arguments")?,
            cap: cap.ok_or("invalid relay arguments")?,
            deadline: Duration::from_millis(millis.ok_or("invalid relay arguments")?.get()),
        };
        if !result.record.is_absolute() {
            return Err("invalid relay arguments");
        }
        Ok(result)
    }
}

pub fn run() -> Result<(), &'static str> {
    let options = Options::parse()?;
    let record = ConnectionRecord::read(&options.record)
        .map_err(|_| "local app unavailable or record invalid")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "local relay runtime failed")?;
    let result = runtime.block_on(bridge(record, options));
    // Tokio stdin may have an uncancellable blocking read. Do not join that
    // thread when the app closes its connection or the other pump fails.
    runtime.shutdown_background();
    result
}

async fn bridge(record: ConnectionRecord, options: Options) -> Result<(), &'static str> {
    let connection = connect_authenticated(&record, options.cap, options.cap, options.deadline)
        .await
        .map_err(|_| "local authentication failed or app unavailable")?;
    let buffered = connection.into_stream().map_err(|_| "local relay failed")?;
    let (read, write) = tokio::io::split(buffered);
    let upstream = pump(tokio::io::stdin(), write, &options, true);
    let downstream = pump(read, tokio::io::stdout(), &options, false);
    let (mut upstream, mut downstream) = (pin!(upstream), pin!(downstream));
    // Clean stdin EOF half-closes IPC writes, then drains pending responses.
    // Other errors or app EOF cancel both directions immediately.
    let (from_stdin, result) = poll_fn(|cx| {
        if let Poll::Ready(result) = upstream.as_mut().poll(cx) {
            return Poll::Ready((true, result));
        }
        downstream.as_mut().poll(cx).map(|result| (false, result))
    })
    .await;
    if from_stdin && result.is_ok() {
        downstream.await
    } else {
        result
    }
}

async fn pump(
    reader: impl AsyncRead + Unpin,
    mut writer: impl AsyncWrite + Unpin,
    options: &Options,
    shutdown_on_eof: bool,
) -> Result<(), &'static str> {
    let mut reader =
        BufReader::with_capacity(options.cap.get().saturating_add(1).min(8192), reader);
    loop {
        let Some(value) = read_frame(&mut reader, options.cap, options.deadline)
            .await
            .map_err(|_| "local relay frame failed")?
        else {
            if shutdown_on_eof {
                tokio::time::timeout(options.deadline, writer.shutdown())
                    .await
                    .map_err(|_| "local relay frame failed")?
                    .map_err(|_| "local relay frame failed")?;
            }
            return Ok(());
        };
        write_frame(&mut writer, &value, options.cap, options.deadline)
            .await
            .map_err(|_| "local relay frame failed")?;
    }
}
