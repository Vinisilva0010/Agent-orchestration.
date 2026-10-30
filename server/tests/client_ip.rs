//! Which address sign-in attempts are counted against.

use std::net::IpAddr;

use axum::http::{HeaderMap, HeaderValue};
use server::http::{client_ip, parse_trusted_proxies};

const PROXY: &str = "10.0.0.5";
const CLIENT: &str = "203.0.113.1";

fn ip(text: &str) -> IpAddr {
    text.parse().unwrap()
}

fn forwarded(values: &[&str]) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for value in values {
        headers.append("x-forwarded-for", HeaderValue::from_str(value).unwrap());
    }
    headers
}

#[test]
fn forwarded_for_from_an_untrusted_peer_is_ignored() {
    let direct = ip("198.51.100.9");
    assert_eq!(
        client_ip(direct, &forwarded(&[CLIENT]), &[ip(PROXY)]),
        direct
    );
    assert_eq!(client_ip(ip(PROXY), &forwarded(&[CLIENT]), &[]), ip(PROXY));
}

#[test]
fn a_trusted_proxy_reports_the_client() {
    assert_eq!(
        client_ip(ip(PROXY), &forwarded(&[CLIENT]), &[ip(PROXY)]),
        ip(CLIENT)
    );
}

#[test]
fn addresses_the_client_wrote_itself_are_skipped() {
    let trusted = [ip(PROXY)];
    let spoofed = forwarded(&["1.1.1.1, 203.0.113.1"]);
    assert_eq!(client_ip(ip(PROXY), &spoofed, &trusted), ip(CLIENT));
    let posing_as_proxy = forwarded(&["10.0.0.5, 203.0.113.1"]);
    assert_eq!(client_ip(ip(PROXY), &posing_as_proxy, &trusted), ip(CLIENT));
}

#[test]
fn a_chain_of_trusted_proxies_is_walked() {
    let trusted = [ip(PROXY), ip("10.0.0.6")];
    assert_eq!(
        client_ip(ip(PROXY), &forwarded(&["203.0.113.1, 10.0.0.6"]), &trusted),
        ip(CLIENT)
    );
    assert_eq!(
        client_ip(ip(PROXY), &forwarded(&[CLIENT, "10.0.0.6"]), &trusted),
        ip(CLIENT)
    );
}

#[test]
fn missing_or_unreadable_headers_fall_back_to_the_peer() {
    let trusted = [ip(PROXY)];
    assert_eq!(client_ip(ip(PROXY), &HeaderMap::new(), &trusted), ip(PROXY));
    assert_eq!(
        client_ip(ip(PROXY), &forwarded(&["not-an-ip"]), &trusted),
        ip(PROXY)
    );
    assert_eq!(
        client_ip(ip(PROXY), &forwarded(&[PROXY]), &trusted),
        ip(PROXY)
    );
}

#[test]
fn ipv4_mapped_peers_match_their_ipv4_form() {
    let mapped = ip("::ffff:10.0.0.5");
    assert_eq!(
        client_ip(mapped, &forwarded(&[CLIENT]), &[ip(PROXY)]),
        ip(CLIENT)
    );
}

#[test]
fn the_proxy_list_is_validated() {
    assert_eq!(
        parse_trusted_proxies("10.0.0.5, ::1").unwrap(),
        vec![ip(PROXY), ip("::1")]
    );
    assert_eq!(
        parse_trusted_proxies("::ffff:10.0.0.5").unwrap(),
        vec![ip(PROXY)]
    );
    assert!(parse_trusted_proxies("").unwrap().is_empty());
    assert!(parse_trusted_proxies("10.0.0.5, banana").is_err());
    let too_many = vec!["10.0.0.1"; 33].join(",");
    assert!(parse_trusted_proxies(&too_many).is_err());
}
