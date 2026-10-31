use std::{
    future::Future,
    io,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Duration,
};

use hickory_resolver::{
    Resolver, TokioResolver,
    config::{CLOUDFLARE, GOOGLE, LookupIpStrategy, ResolveHosts, ResolverConfig},
    net::runtime::TokioRuntimeProvider,
    net::{DnsError, NetError},
    proto::op::ResponseCode,
};
use tokio::time::timeout;

const PROVIDER_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub struct Dns {
    primary: Arc<TokioResolver>,
    fallback: Arc<TokioResolver>,
}

impl Dns {
    pub fn new() -> io::Result<Self> {
        Self::from_configs(
            ResolverConfig::https(&CLOUDFLARE),
            ResolverConfig::https(&GOOGLE),
        )
    }

    fn from_configs(primary: ResolverConfig, fallback: ResolverConfig) -> io::Result<Self> {
        Ok(Self {
            primary: Arc::new(Self::build_resolver(primary)?),
            fallback: Arc::new(Self::build_resolver(fallback)?),
        })
    }

    fn build_resolver(config: ResolverConfig) -> io::Result<TokioResolver> {
        let mut builder = Resolver::builder_with_config(config, TokioRuntimeProvider::default());
        let options = builder.options_mut();
        options.ip_strategy = LookupIpStrategy::Ipv4AndIpv6;
        options.use_hosts_file = ResolveHosts::Never;
        options.timeout = PROVIDER_TIMEOUT;
        options.attempts = 1;
        builder.build().map_err(io::Error::other)
    }

    pub async fn resolve(&self, host: &str, port: u16) -> io::Result<Vec<SocketAddr>> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![SocketAddr::new(ip, port)]);
        }
        // Absolute names avoid search suffixes. Only configured HTTPS servers are used.
        let name = format!("{}.", host.trim_end_matches('.'));
        let lookup = lookup_with_fallback(
            self.primary.lookup_ip(name.clone()),
            self.fallback.lookup_ip(name),
        )
        .await?;
        let addresses: Vec<_> = lookup.iter().map(|ip| SocketAddr::new(ip, port)).collect();
        if addresses.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                "DoH returned no IP addresses",
            ));
        }
        Ok(addresses)
    }
}

async fn lookup_with_fallback<T>(
    primary: impl Future<Output = Result<T, NetError>>,
    fallback: impl Future<Output = Result<T, NetError>>,
) -> io::Result<T> {
    let primary_error = match timeout(PROVIDER_TIMEOUT, primary).await {
        Ok(Ok(answer)) => return Ok(answer),
        Ok(Err(error)) => {
            let definitive = match &error {
                NetError::Dns(DnsError::NoRecordsFound(records)) => {
                    matches!(
                        records.response_code,
                        ResponseCode::NXDomain | ResponseCode::NoError
                    )
                }
                NetError::Dns(DnsError::ResponseCode(ResponseCode::NXDomain)) => true,
                _ => false,
            };
            if definitive {
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("Cloudflare DoH returned a negative DNS answer: {error}"),
                ));
            }
            error.to_string()
        }
        Err(_) => "provider deadline exceeded".to_string(),
    };
    eprintln!("Cloudflare DoH failed: {primary_error}; trying Google DoH");
    match timeout(PROVIDER_TIMEOUT, fallback).await {
        Ok(Ok(answer)) => Ok(answer),
        Ok(Err(error)) => Err(io::Error::other(format!(
            "DoH lookup failed: Cloudflare: {primary_error}; Google: {error}"
        ))),
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "DoH lookup failed: Cloudflare: {primary_error}; Google: provider deadline exceeded"
            ),
        )),
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use hickory_resolver::config::ProtocolConfig;
    use tokio::{io::AsyncWriteExt, net::TcpListener, time::timeout};

    #[test]
    fn providers_use_only_https_with_bootstrap_ips() {
        for provider in [&CLOUDFLARE, &GOOGLE] {
            let config = ResolverConfig::https(provider);
            assert!(config.domain.is_none());
            assert!(config.search.is_empty());
            assert_eq!(config.name_servers.len(), provider.ips.len());
            for server in config.name_servers {
                assert!(provider.ips.contains(&server.ip));
                assert_eq!(server.connections.len(), 1);
                let connection = &server.connections[0];
                assert_eq!(connection.port, 443);
                assert!(matches!(
                    &connection.protocol,
                    ProtocolConfig::Https { server_name, path }
                        if server_name.as_ref() == provider.server_name
                            && path.as_ref() == "/dns-query"
                ));
            }
        }
    }

    #[tokio::test]
    async fn ip_literals_do_not_need_dns() {
        let dns = Dns::new().unwrap();
        for ip in ["127.0.0.1", "::1"] {
            assert_eq!(
                dns.resolve(ip, 8443).await.unwrap(),
                vec![SocketAddr::new(ip.parse().unwrap(), 8443)]
            );
        }
    }

    pub async fn broken_doh() -> (Dns, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let mut config = ResolverConfig::https(&CLOUDFLARE);
        config.name_servers.truncate(1);
        config.name_servers[0].ip = addr.ip();
        config.name_servers[0].connections[0].port = addr.port();
        let server = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let _ = stream.write_all(b"not a TLS server").await;
            }
        });
        (Dns::from_configs(config.clone(), config).unwrap(), server)
    }

    #[tokio::test]
    async fn both_failed_https_providers_return_error() {
        let (dns, server) = broken_doh().await;
        let result = timeout(Duration::from_secs(5), dns.resolve("example.com", 443)).await;
        server.abort();
        let error = result.expect("DNS lookup timed out").unwrap_err();
        assert!(error.to_string().contains("DoH lookup failed"));
    }

    #[tokio::test]
    #[ignore = "requires access to Google DoH"]
    async fn live_google_fallback() {
        let (mut dns, server) = broken_doh().await;
        dns.fallback = Arc::new(Dns::build_resolver(ResolverConfig::https(&GOOGLE)).unwrap());
        let result = timeout(Duration::from_secs(8), dns.resolve("example.com", 443)).await;
        server.abort();
        let addresses = result.expect("failover timed out").unwrap();
        assert!(!addresses.is_empty());
        assert!(addresses.iter().all(|address| address.port() == 443));
    }

    #[tokio::test]
    async fn primary_success_does_not_query_google() {
        let answer = lookup_with_fallback(async { Ok(42) }, async {
            panic!("fallback must not be polled");
        })
        .await
        .unwrap();
        assert_eq!(answer, 42);
    }

    #[tokio::test]
    async fn server_failure_uses_google() {
        let answer = lookup_with_fallback(
            async {
                Err(NetError::Dns(DnsError::ResponseCode(
                    ResponseCode::ServFail,
                )))
            },
            async { Ok(42) },
        )
        .await
        .unwrap();
        assert_eq!(answer, 42);
    }

    #[tokio::test]
    async fn primary_timeout_uses_google() {
        let answer = lookup_with_fallback(std::future::pending(), async { Ok(42) })
            .await
            .unwrap();
        assert_eq!(answer, 42);
    }

    #[tokio::test]
    async fn negative_answers_do_not_query_google() {
        use hickory_resolver::{net::NoRecords, proto::op::Query};
        for code in [ResponseCode::NXDomain, ResponseCode::NoError] {
            let error = NetError::Dns(DnsError::NoRecordsFound(NoRecords::new(Query::new(), code)));
            let result: io::Result<()> = lookup_with_fallback(async { Err(error) }, async {
                panic!("negative answers must not query Google");
            })
            .await;
            assert_eq!(result.unwrap_err().kind(), io::ErrorKind::NotFound);
        }
    }
}
