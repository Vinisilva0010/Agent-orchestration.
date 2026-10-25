//! Main server: authentication, chat, realtime events and audit log.

fn service_name() -> &'static str {
    "server"
}

fn main() {
    println!("{} starting", service_name());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_service_name() {
        assert_eq!(service_name(), "server");
    }
}
