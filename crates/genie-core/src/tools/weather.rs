use std::time::Duration;

use anyhow::Result;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// Weather via Open-Meteo API (free, no API key required).
///
/// Open-Meteo provides current weather and 7-day forecast.
/// We use their geocoding API to resolve city names → coordinates,
/// then fetch weather for those coordinates.
///
/// All requests go through raw TCP+TLS-free HTTP to api.open-meteo.com.
/// Note: Open-Meteo supports HTTP (no TLS required for the free tier).

/// Connect-timeout cap for Open-Meteo HTTP requests.
const WEATHER_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Total request-lifecycle timeout (covers write + read). Without this, a
/// slow or hung Open-Meteo response leaves the calling chat task wedged
/// forever. Same fix shape as PR #174 / closes #173 for `ha::client`.
const WEATHER_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

/// Body-size cap on the accumulated response. The Open-Meteo free tier
/// returns < 4 KiB for geocoding and < 20 KiB for forecasts in practice;
/// 1 MiB leaves a healthy margin while still preventing RSS growth from a
/// misbehaving or man-in-the-middle response (the connection is plain
/// HTTP). Without this the body-read loop would accumulate unboundedly.
const WEATHER_MAX_RESPONSE_BYTES: usize = 1024 * 1024;

// ── Public API ──────────────────────────────────────────────

/// Get current weather for a location.
pub async fn get_weather(location: &str) -> Result<String> {
    // Step 1: Geocode the location name → lat/lon.
    let (lat, lon, resolved_name) = geocode(location).await?;

    // Step 2: Fetch current weather.
    let weather = fetch_weather(lat, lon).await?;

    Ok(format!(
        "Weather in {}: {}°C (feels like {}°C), {}. Wind: {} km/h. Humidity: {}%.",
        resolved_name,
        weather.temperature,
        weather.feels_like,
        weather.description,
        weather.wind_speed,
        weather.humidity,
    ))
}

/// Get weather forecast for a location.
pub async fn get_forecast(location: &str) -> Result<String> {
    let (lat, lon, resolved_name) = geocode(location).await?;
    let forecast = fetch_forecast(lat, lon).await?;

    let mut lines = vec![format!("Forecast for {}:", resolved_name)];
    for day in &forecast {
        lines.push(format!(
            "  {} — {}°C to {}°C, {}",
            day.date, day.temp_min, day.temp_max, day.description
        ));
    }

    Ok(lines.join("\n"))
}

struct CurrentWeather {
    temperature: f64,
    feels_like: f64,
    wind_speed: f64,
    humidity: f64,
    description: String,
}

struct ForecastDay {
    date: String,
    temp_min: f64,
    temp_max: f64,
    description: String,
}

/// Geocode a location name using Open-Meteo's geocoding API.
async fn geocode(location: &str) -> Result<(f64, f64, String)> {
    // Previously `location.replace(' ', "+")` — that only handled spaces,
    // so reserved URL characters in the location (e.g. `&`, `?`, `#`, `=`,
    // `%`, `+`) leaked into the query string and silently broke geocoding.
    // A user asking the weather in "Q&A Cafe Tokyo" used to produce
    // `name=Q&A+Cafe+Tokyo&count=1…` — Open-Meteo parsed the `&` as a
    // separator and saw `name=Q`. Percent-encode every reserved char now.
    let encoded = url_encode_query_param(location);
    let path = format!(
        "/v1/search?name={}&count=1&language=en&format=json",
        encoded
    );

    let body = http_get("geocoding-api.open-meteo.com", &path).await?;
    let data: serde_json::Value = serde_json::from_str(&body)?;

    let results = data
        .get("results")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("location '{}' not found", location))?;

    let first = results
        .first()
        .ok_or_else(|| anyhow::anyhow!("location '{}' not found", location))?;

    let lat = first
        .get("latitude")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let lon = first
        .get("longitude")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let name = first
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or(location)
        .to_string();

    Ok((lat, lon, name))
}

/// Fetch current weather from Open-Meteo.
async fn fetch_weather(lat: f64, lon: f64) -> Result<CurrentWeather> {
    let path = format!(
        "/v1/forecast?latitude={}&longitude={}&current=temperature_2m,relative_humidity_2m,apparent_temperature,weather_code,wind_speed_10m&timezone=auto",
        lat, lon
    );

    let body = http_get("api.open-meteo.com", &path).await?;
    let data: serde_json::Value = serde_json::from_str(&body)?;

    let current = data
        .get("current")
        .ok_or_else(|| anyhow::anyhow!("no current weather data"))?;

    let temperature = current
        .get("temperature_2m")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let feels_like = current
        .get("apparent_temperature")
        .and_then(|v| v.as_f64())
        .unwrap_or(temperature);
    let humidity = current
        .get("relative_humidity_2m")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let wind_speed = current
        .get("wind_speed_10m")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let weather_code = current
        .get("weather_code")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    Ok(CurrentWeather {
        temperature,
        feels_like,
        wind_speed,
        humidity,
        description: wmo_code_to_description(weather_code),
    })
}

/// Fetch 7-day forecast from Open-Meteo.
async fn fetch_forecast(lat: f64, lon: f64) -> Result<Vec<ForecastDay>> {
    let path = format!(
        "/v1/forecast?latitude={}&longitude={}&daily=temperature_2m_max,temperature_2m_min,weather_code&timezone=auto&forecast_days=7",
        lat, lon
    );

    let body = http_get("api.open-meteo.com", &path).await?;
    let data: serde_json::Value = serde_json::from_str(&body)?;

    let daily = data
        .get("daily")
        .ok_or_else(|| anyhow::anyhow!("no forecast data"))?;

    let dates = daily.get("time").and_then(|v| v.as_array());
    let maxs = daily.get("temperature_2m_max").and_then(|v| v.as_array());
    let mins = daily.get("temperature_2m_min").and_then(|v| v.as_array());
    let codes = daily.get("weather_code").and_then(|v| v.as_array());

    let mut forecast = Vec::new();
    if let (Some(dates), Some(maxs), Some(mins), Some(codes)) = (dates, maxs, mins, codes) {
        for i in 0..dates.len().min(7) {
            forecast.push(ForecastDay {
                date: dates[i].as_str().unwrap_or("").to_string(),
                temp_max: maxs[i].as_f64().unwrap_or(0.0),
                temp_min: mins[i].as_f64().unwrap_or(0.0),
                description: wmo_code_to_description(codes[i].as_u64().unwrap_or(0)),
            });
        }
    }

    Ok(forecast)
}

/// Raw HTTP GET (no TLS — Open-Meteo supports plain HTTP).
///
/// Production callers reach this via the host-only convenience signature;
/// it defaults to port 80 and the workspace-wide timeout/size constants.
async fn http_get(host: &str, path: &str) -> Result<String> {
    http_get_with_limits(
        host,
        80,
        path,
        WEATHER_CONNECT_TIMEOUT,
        WEATHER_REQUEST_TIMEOUT,
        WEATHER_MAX_RESPONSE_BYTES,
    )
    .await
}

/// Inner implementation that takes explicit limits — exposed `pub(crate)`
/// only so the test module can point at an ephemeral mock listener with
/// millisecond-scale timeouts. NOT part of any stable API.
pub(crate) async fn http_get_with_limits(
    host: &str,
    port: u16,
    path: &str,
    connect_timeout: Duration,
    request_timeout: Duration,
    max_bytes: usize,
) -> Result<String> {
    let addr = format!("{}:{}", host, port);
    let stream = tokio::time::timeout(connect_timeout, TcpStream::connect(&addr))
        .await
        .map_err(|_| anyhow::anyhow!("Open-Meteo connect to {} timed out", addr))??;

    let (reader, mut writer) = stream.into_split();

    let request = format!(
        "GET {} HTTP/1.1\r\nHost: {}\r\nUser-Agent: GeniePod/0.2\r\nAccept: application/json\r\nConnection: close\r\n\r\n",
        path, host
    );

    // Run the entire write + read cycle under a single deadline. Without
    // this a hung Open-Meteo wedges the chat task forever even after the
    // TCP connect succeeds. Same shape as PR #174 / closes #173.
    let body = tokio::time::timeout(request_timeout, async move {
        writer.write_all(request.as_bytes()).await?;
        read_http_get_body(reader, max_bytes).await
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "Open-Meteo GET {} timed out after {}s",
            path,
            request_timeout.as_secs()
        )
    })??;

    Ok(body.trim().to_string())
}

/// Drain headers and body from a one-shot `Connection: close` GET response,
/// rejecting chunked encoding (Open-Meteo's free tier doesn't use it) and
/// bounding accumulated body bytes at `max_bytes`.
async fn read_http_get_body(
    reader: tokio::net::tcp::OwnedReadHalf,
    max_bytes: usize,
) -> Result<String> {
    let mut buf_reader = BufReader::new(reader);
    let mut in_body = false;
    let mut body = String::new();

    loop {
        let mut line = String::new();
        let n = buf_reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }

        if !in_body {
            // The previous implementation tried to be clever about
            // `Transfer-Encoding: chunked` after the fact (lines 216-225
            // of the pre-fix code) and joined non-hex-only lines with
            // `push_str` — no separator between adjacent chunks, so JSON
            // spanning multiple chunks was silently corrupted, and the
            // heuristic fired on legitimate responses whose first body
            // character was a hex digit. Refuse chunked explicitly
            // instead: Open-Meteo's free tier returns Content-Length, so
            // this path should be unreachable in practice; if a proxy or
            // future Open-Meteo upgrade turns it on, the user gets a
            // clear error and the call site can retry/fallback.
            let lower = line.to_ascii_lowercase();
            if let Some(value) = lower.strip_prefix("transfer-encoding:")
                && value.split(',').any(|tok| tok.trim() == "chunked")
            {
                anyhow::bail!("Open-Meteo response uses unsupported chunked encoding");
            }

            if line.trim().is_empty() {
                in_body = true;
            }
            continue;
        }

        // Body branch.
        let projected = body.len().saturating_add(line.len());
        if projected > max_bytes {
            anyhow::bail!(
                "Open-Meteo response exceeded {} bytes (got at least {})",
                max_bytes,
                projected
            );
        }
        body.push_str(&line);
    }

    Ok(body)
}

/// Percent-encode `s` for safe use as a single query-string parameter
/// value. Encodes every byte that is NOT an unreserved RFC 3986 character
/// (`A-Z`, `a-z`, `0-9`, `-`, `_`, `.`, `~`). Multi-byte UTF-8 codepoints
/// are encoded byte-by-byte, which is what every HTTP server (including
/// Open-Meteo's geocoder) expects. The result is always pure ASCII.
fn url_encode_query_param(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.as_bytes() {
        if matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~') {
            out.push(*byte as char);
        } else {
            out.push('%');
            out.push(hex_nibble(byte >> 4));
            out.push(hex_nibble(byte & 0x0F));
        }
    }
    out
}

fn hex_nibble(n: u8) -> char {
    match n {
        0..=9 => (b'0' + n) as char,
        10..=15 => (b'A' + (n - 10)) as char,
        _ => '0', // unreachable for caller using `b & 0x0F`
    }
}

/// WMO weather interpretation codes → human-readable description.
fn wmo_code_to_description(code: u64) -> String {
    match code {
        0 => "clear sky",
        1 => "mainly clear",
        2 => "partly cloudy",
        3 => "overcast",
        45 | 48 => "foggy",
        51 | 53 | 55 => "drizzle",
        56 | 57 => "freezing drizzle",
        61 | 63 | 65 => "rain",
        66 | 67 => "freezing rain",
        71 | 73 | 75 => "snow",
        77 => "snow grains",
        80..=82 => "rain showers",
        85 | 86 => "snow showers",
        95 => "thunderstorm",
        96 | 99 => "thunderstorm with hail",
        _ => "unknown conditions",
    }
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;

    #[test]
    fn wmo_codes() {
        assert_eq!(wmo_code_to_description(0), "clear sky");
        assert_eq!(wmo_code_to_description(61), "rain");
        assert_eq!(wmo_code_to_description(95), "thunderstorm");
        assert_eq!(wmo_code_to_description(999), "unknown conditions");
    }

    /// Direct unit coverage on the URL-encoding helper. The bug being fixed
    /// is that `location.replace(' ', "+")` only handled spaces; reserved
    /// RFC 3986 characters used to leak into the query string and silently
    /// break geocoding.
    #[test]
    fn url_encode_query_param_percent_encodes_reserved_chars() {
        // Unreserved RFC 3986 characters pass through verbatim.
        assert_eq!(url_encode_query_param("Denver"), "Denver");
        assert_eq!(url_encode_query_param("New-York_2.0~"), "New-York_2.0~");
        // Reserved characters — each must be percent-encoded.
        assert_eq!(url_encode_query_param("Q&A"), "Q%26A");
        assert_eq!(url_encode_query_param("Mom & Pop"), "Mom%20%26%20Pop");
        assert_eq!(url_encode_query_param("a=b"), "a%3Db");
        assert_eq!(url_encode_query_param("a?b"), "a%3Fb");
        assert_eq!(url_encode_query_param("a#b"), "a%23b");
        assert_eq!(url_encode_query_param("a+b"), "a%2Bb");
        assert_eq!(url_encode_query_param("100%"), "100%25");
        assert_eq!(url_encode_query_param("a b"), "a%20b");
        // Multi-byte UTF-8 — each byte of the codepoint encoded.
        // 'ü' is U+00FC, UTF-8 bytes 0xC3 0xBC.
        assert_eq!(url_encode_query_param("München"), "M%C3%BCnchen");
        // Empty string round-trips empty.
        assert_eq!(url_encode_query_param(""), "");
    }

    /// Spawn a `TcpListener` that accepts one connection, drains the
    /// request, and then hangs forever without writing a response. Returns
    /// the local address the client should connect to and a join handle.
    fn spawn_hung_listener() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("local_addr");
        listener.set_nonblocking(true).expect("nonblocking");
        let listener = tokio::net::TcpListener::from_std(listener).expect("from_std");
        let handle = tokio::spawn(async move {
            if let Ok((mut sock, _)) = listener.accept().await {
                // Drain headers so the client thinks the request landed.
                let mut buf = [0u8; 4096];
                let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await;
                // Hang forever (mimics a paused / stuck Open-Meteo).
                tokio::time::sleep(Duration::from_secs(600)).await;
            }
        });
        (addr, handle)
    }

    /// Regression for the timeout fix: a hung Open-Meteo no longer wedges
    /// the chat task. Pre-fix `http_get` blocked indefinitely; with the
    /// fix it returns `Err("…timed out…")` inside the test budget.
    #[tokio::test(flavor = "current_thread")]
    async fn hung_server_after_connect_times_out_cleanly() {
        let (addr, server) = spawn_hung_listener();
        let result = http_get_with_limits(
            &addr.ip().to_string(),
            addr.port(),
            "/v1/search?name=Denver",
            Duration::from_millis(500),
            Duration::from_millis(500),
            WEATHER_MAX_RESPONSE_BYTES,
        )
        .await;
        server.abort();
        let err = result.expect_err("hung server must produce a timeout error");
        let msg = err.to_string();
        assert!(
            msg.contains("timed out"),
            "expected a timeout error, got: {}",
            msg
        );
    }

    /// Regression for the size-cap fix: an oversized response body fails
    /// cleanly with a "exceeded N bytes" error instead of growing RSS.
    #[tokio::test(flavor = "current_thread")]
    async fn oversized_response_is_size_capped() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await;
            // Write headers without `Content-Length`, then stream lots of
            // bytes. The body loop should bail before reading all of it.
            let _ = sock
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n")
                .await;
            let chunk = vec![b'x'; 16 * 1024]; // 16 KiB at a time
            for _ in 0..1024 {
                if sock.write_all(&chunk).await.is_err() {
                    break;
                }
            }
        });
        let result = http_get_with_limits(
            &addr.ip().to_string(),
            addr.port(),
            "/v1/search?name=Denver",
            Duration::from_millis(500),
            Duration::from_secs(5),
            64 * 1024, // cap at 64 KiB so the test bails fast
        )
        .await;
        server.abort();
        let err = result.expect_err("oversized response must produce a size error");
        assert!(
            err.to_string().contains("exceeded"),
            "expected a size-exceeded error, got: {}",
            err
        );
    }

    /// Regression for the chunked-encoding fix: a `Transfer-Encoding:
    /// chunked` response is rejected explicitly instead of going through
    /// the old broken decoder that silently corrupted multi-chunk JSON.
    #[tokio::test(flavor = "current_thread")]
    async fn chunked_encoding_is_explicitly_rejected() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut buf = [0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut sock, &mut buf).await;
            let response = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n7\r\n{\"a\":1}\r\n0\r\n\r\n";
            let _ = sock.write_all(response.as_bytes()).await;
        });
        let result = http_get_with_limits(
            &addr.ip().to_string(),
            addr.port(),
            "/v1/search?name=Denver",
            Duration::from_millis(500),
            Duration::from_secs(5),
            WEATHER_MAX_RESPONSE_BYTES,
        )
        .await;
        server.abort();
        let err = result.expect_err("chunked encoding must produce an explicit error");
        assert!(
            err.to_string().contains("chunked"),
            "expected a chunked-encoding error, got: {}",
            err
        );
    }

    /// Regression for the URL-encoding fix: the geocode request line on
    /// the wire percent-encodes `&` in the location. Pre-fix the unencoded
    /// `&` terminated the `name` query parameter at the HTTP level.
    #[tokio::test(flavor = "current_thread")]
    async fn geocode_request_line_contains_percent_encoded_location() {
        // We rebuild the geocode query string the same way `geocode` does,
        // then verify the wire-level encoding by sending it through a mock
        // server that echoes the first request line back into the body.
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.expect("accept");
            let mut bytes = Vec::new();
            // Read until we see the end-of-headers marker.
            let mut tmp = [0u8; 4096];
            loop {
                match tokio::io::AsyncReadExt::read(&mut sock, &mut tmp).await {
                    Ok(0) => break,
                    Ok(n) => {
                        bytes.extend_from_slice(&tmp[..n]);
                        if bytes.windows(4).any(|w| w == b"\r\n\r\n") {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            // Reflect the captured first line back as a JSON body.
            let first_line = String::from_utf8_lossy(&bytes)
                .lines()
                .next()
                .unwrap_or("")
                .to_string();
            let body = format!(
                "{{\"first_line\":{}}}",
                serde_json::to_string(&first_line).unwrap_or_default()
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            let _ = sock.write_all(response.as_bytes()).await;
        });

        // Construct the same path geocode() builds, using the helper.
        let encoded = url_encode_query_param("Q&A Cafe Tokyo");
        let path = format!(
            "/v1/search?name={}&count=1&language=en&format=json",
            encoded
        );
        let body = http_get_with_limits(
            &addr.ip().to_string(),
            addr.port(),
            &path,
            Duration::from_millis(500),
            Duration::from_secs(5),
            WEATHER_MAX_RESPONSE_BYTES,
        )
        .await
        .expect("mock server must respond");
        server.abort();
        let echo: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        let line = echo
            .get("first_line")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            line.contains("name=Q%26A"),
            "wire-level request line must percent-encode '&'; got: {}",
            line
        );
        assert!(
            !line.contains("name=Q&A"),
            "must NOT contain unencoded '&' inside the name parameter; got: {}",
            line
        );
    }
}
