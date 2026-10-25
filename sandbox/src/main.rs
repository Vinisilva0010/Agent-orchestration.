//! Isolated executor for code and file actions requested by agents.

fn service_name() -> &'static str {
    "sandbox"
}

fn main() {
    println!("{} starting", service_name());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_service_name() {
        assert_eq!(service_name(), "sandbox");
    }
}
