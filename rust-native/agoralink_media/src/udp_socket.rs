pub const DEFAULT_UDP_BUFFER_BYTES: i32 = 64 * 1024 * 1024;
const MIN_UDP_BUFFER_BYTES: i32 = 8 * 1024 * 1024;

#[cfg(windows)]
mod platform {
    use std::io;
    use std::net::UdpSocket;
    use std::os::windows::io::AsRawSocket;

    use windows::core::PSTR;
    use windows::Win32::Networking::WinSock::{
        getsockopt, setsockopt, SOCKET, SOCKET_ERROR, SOL_SOCKET, SO_RCVBUF, SO_SNDBUF,
    };

    fn set_buffer(socket: &UdpSocket, option: i32, bytes: i32, label: &str) -> Result<i32, String> {
        let raw = SOCKET(socket.as_raw_socket() as usize);
        let value = bytes.to_ne_bytes();
        let result = unsafe { setsockopt(raw, SOL_SOCKET, option, Some(&value)) };
        if result == SOCKET_ERROR {
            return Err(format!(
                "set UDP {label} buffer to {bytes} bytes failed: {}",
                io::Error::last_os_error()
            ));
        }
        get_buffer(socket, option, label)
    }

    fn get_buffer(socket: &UdpSocket, option: i32, label: &str) -> Result<i32, String> {
        let raw = SOCKET(socket.as_raw_socket() as usize);
        let mut value = 0i32;
        let mut length = std::mem::size_of::<i32>() as i32;
        let result = unsafe {
            getsockopt(
                raw,
                SOL_SOCKET,
                option,
                PSTR((&mut value as *mut i32).cast::<u8>()),
                &mut length,
            )
        };
        if result == SOCKET_ERROR {
            Err(format!(
                "read UDP {label} buffer size failed: {}",
                io::Error::last_os_error()
            ))
        } else {
            Ok(value)
        }
    }

    pub fn configure_send_buffer(socket: &UdpSocket, bytes: i32) -> Result<i32, String> {
        configure_buffer(socket, SO_SNDBUF, bytes, "send")
    }

    pub fn configure_receive_buffer(socket: &UdpSocket, bytes: i32) -> Result<i32, String> {
        configure_buffer(socket, SO_RCVBUF, bytes, "receive")
    }

    fn configure_buffer(
        socket: &UdpSocket,
        option: i32,
        bytes: i32,
        label: &str,
    ) -> Result<i32, String> {
        match set_buffer(socket, option, bytes, label) {
            Ok(actual) => Ok(actual),
            Err(primary) if bytes > super::MIN_UDP_BUFFER_BYTES => {
                match set_buffer(socket, option, super::MIN_UDP_BUFFER_BYTES, label) {
                    Ok(actual) => Ok(actual),
                    Err(fallback) => get_buffer(socket, option, label).map_err(|read_error| {
                        format!(
                            "{primary}; fallback to {} bytes also failed: {fallback}; unable to read existing buffer: {read_error}",
                            super::MIN_UDP_BUFFER_BYTES
                        )
                    }),
                }
            }
            Err(error) => get_buffer(socket, option, label).map_err(|read_error| {
                format!("{error}; unable to read existing UDP {label} buffer: {read_error}")
            }),
        }
    }
}

#[cfg(windows)]
pub use platform::{configure_receive_buffer, configure_send_buffer};

#[cfg(not(windows))]
pub fn configure_send_buffer(_socket: &std::net::UdpSocket, bytes: i32) -> Result<i32, String> {
    Ok(bytes)
}

#[cfg(not(windows))]
pub fn configure_receive_buffer(_socket: &std::net::UdpSocket, bytes: i32) -> Result<i32, String> {
    Ok(bytes)
}
