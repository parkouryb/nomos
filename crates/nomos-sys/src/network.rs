use nomos_core::lease::NetworkMode;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};
use tracing::info;

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
    is_cgroup_available: bool,
}

impl Default for NetworkIsolationHelper {
    fn default() -> Self {
        Self::new(PathBuf::from("/sys/fs/cgroup/nomos"))
    }
}

impl NetworkIsolationHelper {
    pub fn new(base_cgroup_path: PathBuf) -> Self {
        let is_available = base_cgroup_path.exists() || Path::new("/sys/fs/cgroup").exists();
        Self {
            base_cgroup_path,
            is_cgroup_available: is_available,
        }
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

    /// Sets up the cgroup directory structure for a lease under its network isolation mode.
    pub fn setup_lease_slice(&self, lease_id: &str, mode: NetworkMode) -> Result<PathBuf, std::io::Error> {
        let slice = self.slice_path_for_lease(lease_id, mode);
        if !self.is_cgroup_available {
            info!(
                "[Mock Network Isolation] Configured virtual slice {} (mode: {:?})",
                lease_id, mode
            );
            return Ok(slice);
        }

        let mode_dir = self.mode_cgroup_path(mode);
        fs::create_dir_all(&mode_dir)?;

        // Enable subtree control in parent and mode dir if available
        let _ = fs::write(self.base_cgroup_path.join("cgroup.subtree_control"), "+cpu +memory +io");
        let _ = fs::write(mode_dir.join("cgroup.subtree_control"), "+cpu +memory +io");

        fs::create_dir_all(&slice)?;
        info!(
            "Established cgroup slice for lease {} under network mode '{}': {:?}",
            lease_id, mode, slice
        );
        Ok(slice)
    }

    /// Attaches a process ID to the corresponding cgroup slice.
    pub fn attach_pid(&self, lease_id: &str, mode: NetworkMode, pid: u32) -> Result<(), std::io::Error> {
        let slice = self.slice_path_for_lease(lease_id, mode);
        if !self.is_cgroup_available {
            info!(
                "[Mock Network Isolation] Attached PID {} to virtual slice {} (mode: {:?})",
                pid, lease_id, mode
            );
            return Ok(());
        }

        let procs_file = slice.join("cgroup.procs");
        if !procs_file.exists() {
            // Attempt fallback to root base slice if mode dir doesn't exist
            let fallback = self.base_cgroup_path.join(lease_id).join("cgroup.procs");
            if fallback.exists() {
                return fs::write(fallback, pid.to_string());
            }
        }
        fs::write(procs_file, pid.to_string())
    }

    /// Locates the lease slice across all network mode directories.
    pub fn find_lease_slice(&self, lease_id: &str) -> Option<(PathBuf, NetworkMode)> {
        for mode in [NetworkMode::Isolated, NetworkMode::None, NetworkMode::Host] {
            let candidate = self.slice_path_for_lease(lease_id, mode);
            if candidate.exists() {
                return Some((candidate, mode));
            }
        }
        // Check legacy flat path
        let legacy = self.base_cgroup_path.join(lease_id);
        if legacy.exists() {
            return Some((legacy, NetworkMode::Isolated));
        }
        None
    }

    /// Generates the standard Linux iptables rules for Nomos network isolation.
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
