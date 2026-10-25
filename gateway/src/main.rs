//! Provider gateway: the only component allowed to hold provider API keys.

fn service_name() -> &'static str {
    "gateway"
}

fn main() {
    println!("{} starting", service_name());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_service_name() {
        assert_eq!(service_name(), "gateway");
    }
}
