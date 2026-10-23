use nomos_core::lease::NetworkMode;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::PathBuf;

/// Check if an IPv4 address belongs to RFC 1918 private address space or Link-Local.
///
/// RFC 1918 subnets:
/// - 10.0.0.0/8 (10.0.0.0 - 10.255.255.255)
/// - 172.16.0.0/12 (172.16.0.0 - 172.31.255.255)
/// - 192.168.0.0/16 (192.168.0.0 - 192.168.255.255)
///
/// Link-Local subnet:
/// - 169.254.0.0/16 (169.254.0.0 - 169.254.255.255)
pub fn is_rfc1918_or_local_ipv4(ip: &Ipv4Addr) -> bool {
    let octets = ip.octets();
    match octets[0] {
        10 => true,
        172 if (16..=31).contains(&octets[1]) => true,
        192 if octets[1] == 168 => true,
        169 if octets[1] == 254 => true,
        _ => false,
    }
}

/// Check if an IPv6 address belongs to Unique Local Address (fc00::/7) or Link-Local (fe80::/10).
pub fn is_private_or_local_ipv6(ip: &Ipv6Addr) -> bool {
    let segments = ip.segments();
    // Unique Local Addresses: fc00::/7 (fc00... or fd00...)
    if (segments[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // Link-Local Unicast: fe80::/10
    if (segments[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    false
}

/// Check whether an IP address is considered a private / LAN address.
pub fn is_private_or_lan(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_rfc1918_or_local_ipv4(v4),
        IpAddr::V6(v6) => is_private_or_local_ipv6(v6),
    }
}

/// Evaluates if an egress connection to the specified target IP address is permitted
/// under the given `NetworkMode`.
///
/// Rules:
/// - `NetworkMode::Host`: All destinations are allowed (unrestricted host networking).
/// - `NetworkMode::Isolated`: Allows loopback (127.0.0.1, ::1) and LAN internal networks
///   (RFC 1918: 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, and link-local).
///   Blocks all public Internet WAN addresses.
/// - `NetworkMode::None`: Air-gapped mode. Only loopback connections (127.0.0.1, ::1)
///   are permitted. All external LAN and WAN outbound traffic is blocked.
pub fn is_destination_allowed(mode: NetworkMode, ip: &IpAddr) -> bool {
    match mode {
        NetworkMode::Host => true,
        NetworkMode::None => ip.is_loopback(),
        NetworkMode::Isolated => ip.is_loopback() || is_private_or_lan(ip),
    }
}

/// Helper struct for managing Linux Network Isolation and Cgroups v2 layout.
#[derive(Debug, Clone)]
pub struct NetworkIsolationHelper {
    base_cgroup_path: PathBuf,
}

impl Default for NetworkIsolationHelper {
    fn default() -> Self {
        Self::new(PathBuf::from("/sys/fs/cgroup/nomos"))
    }
}

impl NetworkIsolationHelper {
    pub fn new(base_cgroup_path: PathBuf) -> Self {
        Self { base_cgroup_path }
    }

    /// Returns the subdirectory corresponding to the network mode (e.g. `/sys/fs/cgroup/nomos/isolated`).
    pub fn mode_cgroup_path(&self, mode: NetworkMode) -> PathBuf {
        self.base_cgroup_path.join(mode.as_str())
    }

    /// Returns the full cgroup slice path for a given lease under its network isolation mode.
    /// Hierarchy: `/sys/fs/cgroup/nomos/<mode>/<lease_id>`
    pub fn slice_path_for_lease(&self, lease_id: &str, mode: NetworkMode) -> PathBuf {
        self.mode_cgroup_path(mode).join(lease_id)
    }

    /// This legacy helper cannot enforce networking. Use the resource cgroup runner.
    pub fn setup_lease_slice(&self, _lease_id: &str, _mode: NetworkMode) -> Result<PathBuf, std::io::Error> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Network isolation is not implemented; use the enforced CPU/RAM runner with explicit host networking"))
    }

    pub fn attach_pid(&self, _lease_id: &str, _mode: NetworkMode, _pid: u32) -> Result<(), std::io::Error> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "Legacy network attachment is unsupported; use the enforced runner"))
    }

    pub fn find_lease_slice(&self, lease_id: &str) -> Option<(PathBuf, NetworkMode)> {
        if !lease_id.starts_with("ls-") || !lease_id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') { return None; }
        let candidate = self.slice_path_for_lease(lease_id, NetworkMode::Host);
        crate::cgroup::require_cgroup2(&candidate).ok()?;
        Some((candidate, NetworkMode::Host))
    }

    /// Generates an unapplied IPv4 policy proposal; this does not enforce network isolation.
    /// These rules leverage `-m cgroup --path` against Cgroups v2.
    pub fn generate_iptables_rules(cgroup_base_rel: &str) -> Vec<String> {
        let isolated_cgroup = format!("{}/isolated", cgroup_base_rel.trim_matches('/'));
        let none_cgroup = format!("{}/none", cgroup_base_rel.trim_matches('/'));

        vec![
            // Create dedicated NOMOS_EGRESS chain if not exists
            "iptables -N NOMOS_EGRESS 2>/dev/null || true".to_string(),
            "iptables -C OUTPUT -j NOMOS_EGRESS 2>/dev/null || iptables -A OUTPUT -j NOMOS_EGRESS".to_string(),
            "iptables -F NOMOS_EGRESS".to_string(),

            // Established connections are always allowed
            "iptables -A NOMOS_EGRESS -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT".to_string(),

            // Mode None (Air-gapped): Allow loopback only, drop all external
            format!("iptables -A NOMOS_EGRESS -m cgroup --path \"{}\" -o lo -j ACCEPT", none_cgroup),
            format!("iptables -A NOMOS_EGRESS -m cgroup --path \"{}\" -d 127.0.0.0/8 -j ACCEPT", none_cgroup),
            format!("iptables -A NOMOS_EGRESS -m cgroup --path \"{}\" -j REJECT --reject-with icmp-admin-prohibited", none_cgroup),

            // Mode Isolated (LAN & Localhost): Allow loopback + RFC1918 LAN, drop WAN
            format!("iptables -A NOMOS_EGRESS -m cgroup --path \"{}\" -o lo -j ACCEPT", isolated_cgroup),
            format!("iptables -A NOMOS_EGRESS -m cgroup --path \"{}\" -d 127.0.0.0/8 -j ACCEPT", isolated_cgroup),
            format!("iptables -A NOMOS_EGRESS -m cgroup --path \"{}\" -d 10.0.0.0/8 -j ACCEPT", isolated_cgroup),
            format!("iptables -A NOMOS_EGRESS -m cgroup --path \"{}\" -d 172.16.0.0/12 -j ACCEPT", isolated_cgroup),
            format!("iptables -A NOMOS_EGRESS -m cgroup --path \"{}\" -d 192.168.0.0/16 -j ACCEPT", isolated_cgroup),
            format!("iptables -A NOMOS_EGRESS -m cgroup --path \"{}\" -d 169.254.0.0/16 -j ACCEPT", isolated_cgroup),
            format!("iptables -A NOMOS_EGRESS -m cgroup --path \"{}\" -j REJECT --reject-with icmp-admin-prohibited", isolated_cgroup),
        ]
    }

    /// Generates nftables configuration for Cgroups v2 socket matching.
    pub fn generate_nftables_config(table_name: &str, cgroup_base_rel: &str) -> String {
        let isolated_cgroup = format!("{}/isolated", cgroup_base_rel.trim_matches('/'));
        let none_cgroup = format!("{}/none", cgroup_base_rel.trim_matches('/'));

        format!(
            r#"table inet {table_name} {{
    chain output {{
        type filter hook output priority 0; policy accept;

        # Keep existing established sessions
        ct state established,related accept

        # Air-gapped Mode (none): Only loopback
        socket cgroupv2 "{none_cgroup}" oif "lo" accept
        socket cgroupv2 "{none_cgroup}" ip daddr 127.0.0.0/8 accept
        socket cgroupv2 "{none_cgroup}" reject with icmpx type admin-prohibited

        # Isolated Mode: Loopback and RFC1918 LAN only
        socket cgroupv2 "{isolated_cgroup}" oif "lo" accept
        socket cgroupv2 "{isolated_cgroup}" ip daddr {{ 127.0.0.0/8, 10.0.0.0/8, 172.16.0.0/12, 192.168.0.0/16, 169.254.0.0/16 }} accept
        socket cgroupv2 "{isolated_cgroup}" reject with icmpx type admin-prohibited
    }}
}}
"#,
            table_name = table_name,
            none_cgroup = none_cgroup,
            isolated_cgroup = isolated_cgroup
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn legacy_helper_never_claims_enforcement() {
        let helper = NetworkIsolationHelper::new(PathBuf::from("/nonexistent/nomos"));
        for mode in [NetworkMode::Host, NetworkMode::None, NetworkMode::Isolated] {
            assert!(helper.setup_lease_slice("ls-test", mode.clone()).is_err());
            assert!(helper.attach_pid("ls-test", mode, 1).is_err());
        }
        assert!(helper.find_lease_slice("../escape").is_none());
    }

    #[test]
    fn test_rfc1918_ipv4_detection() {
        // 10.0.0.0/8
        assert!(is_rfc1918_or_local_ipv4(&Ipv4Addr::new(10, 0, 0, 1)));
        assert!(is_rfc1918_or_local_ipv4(&Ipv4Addr::new(10, 255, 255, 254)));

        // 172.16.0.0/12
        assert!(is_rfc1918_or_local_ipv4(&Ipv4Addr::new(172, 16, 0, 1)));
        assert!(is_rfc1918_or_local_ipv4(&Ipv4Addr::new(172, 31, 255, 255)));
        assert!(!is_rfc1918_or_local_ipv4(&Ipv4Addr::new(172, 15, 255, 255)));
        assert!(!is_rfc1918_or_local_ipv4(&Ipv4Addr::new(172, 32, 0, 1)));

        // 192.168.0.0/16
        assert!(is_rfc1918_or_local_ipv4(&Ipv4Addr::new(192, 168, 1, 1)));
        assert!(is_rfc1918_or_local_ipv4(&Ipv4Addr::new(192, 168, 100, 254)));
        assert!(!is_rfc1918_or_local_ipv4(&Ipv4Addr::new(192, 169, 1, 1)));

        // Link-local
        assert!(is_rfc1918_or_local_ipv4(&Ipv4Addr::new(169, 254, 1, 1)));

        // Public WAN IPs
        assert!(!is_rfc1918_or_local_ipv4(&Ipv4Addr::new(8, 8, 8, 8)));
        assert!(!is_rfc1918_or_local_ipv4(&Ipv4Addr::new(1, 1, 1, 1)));
        assert!(!is_rfc1918_or_local_ipv4(&Ipv4Addr::new(142, 250, 190, 46)));
    }

    #[test]
    fn test_mode_none_rules() {
        let lo = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 50));
        let wan = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));

        assert!(is_destination_allowed(NetworkMode::None, &lo));
        assert!(!is_destination_allowed(NetworkMode::None, &lan));
        assert!(!is_destination_allowed(NetworkMode::None, &wan));
    }

    #[test]
    fn test_mode_isolated_rules() {
        let lo = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let lan1 = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let lan2 = IpAddr::V4(Ipv4Addr::new(10, 20, 30, 40));
        let lan3 = IpAddr::V4(Ipv4Addr::new(172, 20, 0, 10));
        let wan1 = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));
        let wan2 = IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1));

        // Isolated allows loopback and LAN
        assert!(is_destination_allowed(NetworkMode::Isolated, &lo));
        assert!(is_destination_allowed(NetworkMode::Isolated, &lan1));
        assert!(is_destination_allowed(NetworkMode::Isolated, &lan2));
        assert!(is_destination_allowed(NetworkMode::Isolated, &lan3));

        // Isolated strictly blocks WAN Internet
        assert!(!is_destination_allowed(NetworkMode::Isolated, &wan1));
        assert!(!is_destination_allowed(NetworkMode::Isolated, &wan2));
    }

    #[test]
    fn test_mode_host_rules() {
        let lo = IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1));
        let lan = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1));
        let wan = IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8));

        // Host mode allows all traffic
        assert!(is_destination_allowed(NetworkMode::Host, &lo));
        assert!(is_destination_allowed(NetworkMode::Host, &lan));
        assert!(is_destination_allowed(NetworkMode::Host, &wan));
    }

    #[test]
    fn test_rules_generation() {
        let iptables = NetworkIsolationHelper::generate_iptables_rules("nomos");
        assert!(iptables.iter().any(|r| r.contains("nomos/isolated") && r.contains("192.168.0.0/16")));
        assert!(iptables.iter().any(|r| r.contains("nomos/none") && r.contains("REJECT")));

        let nftables = NetworkIsolationHelper::generate_nftables_config("nomos_filter", "nomos");
        assert!(nftables.contains("nomos/isolated"));
        assert!(nftables.contains("nomos/none"));
    }
}
