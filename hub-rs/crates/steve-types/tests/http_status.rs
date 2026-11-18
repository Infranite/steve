use steve_types::Code;

#[test]
fn every_http_error_status_uses_the_edge_fallback() {
    for status in 400..=599 {
        let expected = match status {
            401 => Code::Unauthenticated,
            403 => Code::Forbidden,
            404 => Code::NotFound,
            409 => Code::Conflict,
            413 => Code::TooLarge,
            429 => Code::RateLimited,
            501 => Code::Unsupported,
            503 => Code::Unavailable,
            504 => Code::Timeout,
            400..=499 => Code::Invalid,
            _ => Code::Internal,
        };
        assert_eq!(Code::from_http_status(status), expected, "{status}");
    }
}
