//! Length-framed Unix messages share one absolute deadline across connection and I/O.

use serde::{
    de::{self, DeserializeOwned, DeserializeSeed, MapAccess, SeqAccess, Visitor},
    Serialize,
};
use serde_json::{Map, Number, Value};
use std::{
    fmt,
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, net::UnixStream},
    },
    path::Path,
    thread,
    time::{Duration, Instant},
};

pub(crate) const MAX_FRAME: usize = 2 * 1024 * 1024;
const MAX_DEPTH: usize = 40;

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn remaining(deadline: Instant) -> io::Result<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "socket deadline exceeded"))
}

fn transfer(
    stream: &mut UnixStream,
    buffer: &mut [u8],
    deadline: Instant,
    writing: bool,
) -> io::Result<()> {
    let mut offset = 0;
    while offset < buffer.len() {
        let budget = remaining(deadline)?;
        let result = if writing {
            stream.set_write_timeout(Some(budget))?;
            stream.write(&buffer[offset..])
        } else {
            stream.set_read_timeout(Some(budget))?;
            stream.read(&mut buffer[offset..])
        };
        match result {
            Ok(0) => {
                let kind = if writing {
                    io::ErrorKind::WriteZero
                } else {
                    io::ErrorKind::UnexpectedEof
                };
                return Err(io::Error::new(kind, "socket closed before complete frame"));
            }
            Ok(count) => offset += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "socket deadline exceeded",
                ));
            }
            Err(error) => return Err(error),
        }
    }
    // Kernel socket timeouts round up; a late transfer must not become success.
    remaining(deadline).map(|_| ())
}

pub(crate) fn read<T: DeserializeOwned>(
    stream: &mut UnixStream,
    deadline: Instant,
) -> io::Result<T> {
    let mut prefix = [0; 4];
    transfer(stream, &mut prefix, deadline, false)?;
    let length = u32::from_be_bytes(prefix) as usize;
    if length > MAX_FRAME {
        return Err(invalid("socket frame exceeds size limit"));
    }
    let mut bytes = vec![0; length];
    transfer(stream, &mut bytes, deadline, false)?;
    let result = decode(&bytes)?;
    remaining(deadline)?;
    Ok(result)
}

pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> io::Result<T> {
    if bytes.len() > MAX_FRAME {
        return Err(invalid("JSON message exceeds size limit"));
    }
    check_integer_range(bytes)?;
    let mut decoder = serde_json::Deserializer::from_slice(bytes);
    let value = Checked(0)
        .deserialize(&mut decoder)
        .map_err(|_| invalid("expected bounded JSON without duplicate fields"))?;
    decoder
        .end()
        .map_err(|_| invalid("unexpected trailing JSON data"))?;
    serde_json::from_value(value).map_err(|_| invalid("JSON message does not match the protocol"))
}

pub(crate) fn write<T: Serialize>(
    stream: &mut UnixStream,
    value: &T,
    deadline: Instant,
) -> io::Result<()> {
    remaining(deadline)?;
    let mut bytes = BoundedBuffer(Vec::new());
    serde_json::to_writer(&mut bytes, value)
        .map_err(|_| invalid("socket message cannot be encoded within size limit"))?;
    transfer(
        stream,
        &mut (bytes.0.len() as u32).to_be_bytes(),
        deadline,
        true,
    )?;
    transfer(stream, &mut bytes.0, deadline, true)
}

struct BoundedBuffer(Vec<u8>);

impl Write for BoundedBuffer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_FRAME - self.0.len() {
            return Err(invalid("socket frame exceeds size limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

pub(crate) fn connect(path: &Path, deadline: Instant) -> io::Result<UnixStream> {
    remaining(deadline)?;
    let bytes = path.as_os_str().as_bytes();
    // All-zero sockaddr_un is valid before its address fields are initialized.
    let mut address = unsafe { std::mem::zeroed::<libc::sockaddr_un>() };
    if !path.is_absolute()
        || bytes.is_empty()
        || bytes.len() >= address.sun_path.len()
        || bytes.contains(&0)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected an absolute Unix socket path within the platform limit",
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (target, byte) in address.sun_path.iter_mut().zip(bytes) {
        *target = *byte as libc::c_char;
    }
    let length =
        (std::mem::offset_of!(libc::sockaddr_un, sun_path) + bytes.len() + 1) as libc::socklen_t;
    // A successful descriptor immediately transfers to UnixStream ownership.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    stream.set_nonblocking(true)?;
    loop {
        let budget = remaining(deadline)?;
        // The pointer references an initialized sockaddr_un of the supplied length.
        let result = unsafe {
            libc::connect(
                stream.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                length,
            )
        };
        if result == 0 {
            remaining(deadline)?;
            stream.set_nonblocking(false)?;
            return Ok(stream);
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::Interrupted {
            continue;
        }
        if error.kind() != io::ErrorKind::WouldBlock {
            return Err(error);
        }
        // Linux AF_UNIX uses EAGAIN for a full backlog, leaving this socket
        // unconnected. Retrying is bounded by the original connection deadline.
        thread::sleep(Duration::from_millis(5).min(budget));
    }
}

struct Checked(usize);

impl<'de> DeserializeSeed<'de> for Checked {
    type Value = Value;

    fn deserialize<D: de::Deserializer<'de>>(self, decoder: D) -> Result<Value, D::Error> {
        decoder.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Checked {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        formatter.write_str("bounded, unambiguous JSON")
    }

    fn visit_bool<E: de::Error>(self, value: bool) -> Result<Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_unit<E: de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_str<E: de::Error>(self, value: &str) -> Result<Value, E> {
        Ok(Value::String(value.into()))
    }

    fn visit_i64<E: de::Error>(self, value: i64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_u64<E: de::Error>(self, value: u64) -> Result<Value, E> {
        Ok(Value::Number(value.into()))
    }

    fn visit_f64<E: de::Error>(self, value: f64) -> Result<Value, E> {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| de::Error::custom("non-finite number"))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut sequence: A) -> Result<Value, A::Error> {
        if self.0 >= MAX_DEPTH {
            return Err(de::Error::custom("JSON nesting limit"));
        }
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element_seed(Checked(self.0 + 1))? {
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        if self.0 >= MAX_DEPTH {
            return Err(de::Error::custom("JSON nesting limit"));
        }
        let mut values = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom("duplicate JSON field"));
            }
            values.insert(key, map.next_value_seed(Checked(self.0 + 1))?);
        }
        Ok(Value::Object(values))
    }
}

fn check_integer_range(bytes: &[u8]) -> io::Result<()> {
    // Match the Provider boundary: overflowing integer tokens cannot silently
    // become rounded floats; numeric-looking strings remain opaque.
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'"' => {
                index += 1;
                while index < bytes.len() && bytes[index] != b'"' {
                    index += if bytes[index] == b'\\' { 2 } else { 1 };
                }
                index += 1;
            }
            b'-' | b'0'..=b'9' => {
                let start = index;
                while index < bytes.len()
                    && matches!(bytes[index], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
                {
                    index += 1;
                }
                let token = &bytes[start..index];
                if !token.iter().any(|byte| matches!(byte, b'.' | b'e' | b'E')) {
                    let token =
                        std::str::from_utf8(token).map_err(|_| invalid("invalid numeric token"))?;
                    let fits = if token.starts_with('-') {
                        token.parse::<i64>().is_ok()
                    } else {
                        token.parse::<u64>().is_ok()
                    };
                    if !fits {
                        return Err(invalid("integer is outside the supported range"));
                    }
                }
            }
            _ => index += 1,
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn receive(bytes: &[u8]) -> io::Result<Value> {
        let (mut sender, mut receiver) = UnixStream::pair()?;
        sender.write_all(&(bytes.len() as u32).to_be_bytes())?;
        sender.write_all(bytes)?;
        read(&mut receiver, Instant::now() + Duration::from_secs(1))
    }

    #[test]
    fn accepts_native_numbers_unicode_and_escaped_keys() {
        let value =
            receive(br#"{"a":1.25,"b":1e-2,"c":"\u4e2d","d":"18446744073709551616"}"#).unwrap();
        assert_eq!(value["a"], json!(1.25));
        assert_eq!(value["b"], json!(0.01));
        assert_eq!(value["c"], json!("中"));
    }

    #[test]
    fn rejects_duplicates_trailing_and_overflowing_integers() {
        for bytes in [
            br#"{"a":1,"a":2}"#.as_slice(),
            br#"{"n":{"a":1,"\u0061":2}}"#,
            b"{} []",
            b"18446744073709551616",
            b"-9223372036854775809",
            b"",
        ] {
            assert_eq!(
                receive(bytes).unwrap_err().kind(),
                io::ErrorKind::InvalidData
            );
        }
    }

    #[test]
    fn enforces_exact_container_depth_limit() {
        let valid = format!("{}0{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        assert!(receive(valid.as_bytes()).is_ok());
        let invalid = format!("[{valid}]");
        assert_eq!(
            receive(invalid.as_bytes()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn rejects_oversized_prefix_without_waiting_for_payload() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender
            .write_all(&((MAX_FRAME + 1) as u32).to_be_bytes())
            .unwrap();
        let error =
            read::<Value>(&mut receiver, Instant::now() + Duration::from_secs(1)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn rejects_truncated_prefix_and_payload() {
        for bytes in [&[0, 0][..], &[0, 0, 0, 5, b'{', b'}'][..]] {
            let (mut sender, mut receiver) = UnixStream::pair().unwrap();
            sender.write_all(bytes).unwrap();
            drop(sender);
            let error =
                read::<Value>(&mut receiver, Instant::now() + Duration::from_secs(1)).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        }
    }

    #[test]
    fn shares_deadline_between_prefix_and_payload() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        sender.write_all(&2_u32.to_be_bytes()).unwrap();
        let start = Instant::now();
        let error = read::<Value>(&mut receiver, start + Duration::from_millis(25)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn writes_round_trip_and_checks_bounds_before_sending() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        let value = json!({"tool": "read", "value": 1.2});
        write(&mut sender, &value, deadline).unwrap();
        assert_eq!(read::<Value>(&mut receiver, deadline).unwrap(), value);
        let error = write(&mut sender, &"x".repeat(MAX_FRAME), deadline).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        receiver.set_nonblocking(true).unwrap();
        assert_eq!(
            receiver.read(&mut [0; 4]).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
    }

    #[test]
    fn rejects_expired_deadlines_and_invalid_connect_paths() {
        let (mut sender, mut receiver) = UnixStream::pair().unwrap();
        let deadline = Instant::now();
        assert_eq!(
            write(&mut sender, &json!({}), deadline).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        assert_eq!(
            read::<Value>(&mut receiver, deadline).unwrap_err().kind(),
            io::ErrorKind::TimedOut
        );
        for path in ["relative", "/has\0nul", &format!("/{}", "a".repeat(200))] {
            assert_eq!(
                connect(Path::new(path), Instant::now() + Duration::from_secs(1))
                    .unwrap_err()
                    .kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }
}
