use std::{
    fs::File,
    io::{self, BufReader},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    path::Path,
    sync::Arc,
};

use qbase::config::Parameters;
use qconnection::connection::QuicConnection;
use rustls::{
    client::WantsClientCert, ClientConfig as TlsClientConfig, ConfigBuilder, WantsVerifier,
};

type TlsClientConfigBuilder<T> = ConfigBuilder<TlsClientConfig, T>;

/// 其实是一个Builder，最终得到一个ArcConnection
pub struct QuicClient {
    addresses: Vec<SocketAddr>,
    _reuse_connection: bool,
    _enable_happy_eyepballs: bool,
    _prefered_versions: Vec<u32>,
    parameters: Parameters,
    tls_config: Arc<TlsClientConfig>,
}

impl QuicClient {
    /// 无论向哪里发起连接，都使用同一个本地的Usc，包括一对v4和v6的，这在P2P场景下很有用
    pub fn solo() -> QuicClientBuilder<TlsClientConfigBuilder<WantsVerifier>> {
        QuicClient::bind([
            SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
        ])
    }

    /// 绑定一个地址，若该地址
    /// - 已经有了usc(UdpSocket controller)，且已在注册管理中，那就查找即可
    /// - 要是没有查到，那就新建一个usc，然后注册管理起来
    ///
    /// 为何是绑定一系列地址，因为QUIC本身就是支持多路径的。
    /// 况且，为了推广IPv6，通常都是IPv6、IPv4双栈的的Happly Eyeballs策略。
    ///
    /// # Examples
    /// ```
    /// use quic::QuicClient;
    /// use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
    ///
    /// rustls::crypto::ring::default_provider()
    ///     .install_default()
    ///     .expect("Failed to install rustls crypto provider");
    ///
    /// let client_builder = QuicClient::bind([
    ///         "[2001:db8::1]:8080".parse().unwrap(),
    ///         "127.0.0.1:8080".parse().unwrap(),
    ///     ])
    ///     .reuse_connection()
    ///     .enable_happy_eyeballs()
    ///     .prefer_versions([0x00000001u32]);
    /// ```
    pub fn bind(
        addresses: impl IntoIterator<Item = SocketAddr>,
    ) -> QuicClientBuilder<TlsClientConfigBuilder<WantsVerifier>> {
        QuicClientBuilder {
            addresses: addresses.into_iter().collect(),
            reuse_connection: true,
            enable_happy_eyepballs: false,
            preferred_versions: vec![1],
            parameters: Parameters::default(),
            tls_config: TlsClientConfig::builder_with_protocol_versions(&[&rustls::version::TLS13]),
        }
    }

    /// 重新绑定地址，其后创建的连接，会使用新的绑定地址
    pub fn rebind(&mut self, addresses: impl IntoIterator<Item = SocketAddr>) {
        self.addresses.clear();
        self.addresses.extend(addresses);
    }

    /// 使用QuicClient的usc，去创建一个QuicConnection
    /// 需要注意，usc的地址是v4还是v6的，要跟server_addr保持一致
    /// server_name要填写在ClientHello中，
    /// server_addr是目标地址，虽然可以从server_name域名解析出来，但是指定使用哪一个，仍有开发者自己决定
    /// parameters是连接参数，将使用QuicClient中设置好的。
    /// token则根据[`with_token_registry`]设置的方法，来决定是否需要填写
    /// 创建好的连接，应要保存在全局QuicConnection集合中
    /// 那如果开启了reuse_connection选项，则会优先从该全局QuicConnection集合里获取到server_name的
    pub fn connect(
        &self,
        server_name: String,
        server_addr: SocketAddr,
    ) -> io::Result<QuicConnection> {
        Ok(QuicConnection::new_client(
            server_name,
            self.tls_config.clone(),
            server_addr,
            None,
            &self.parameters,
        ))
    }
}

pub struct QuicClientBuilder<T> {
    addresses: Vec<SocketAddr>,
    reuse_connection: bool,
    enable_happy_eyepballs: bool,
    preferred_versions: Vec<u32>,
    parameters: Parameters,
    tls_config: T,
}

impl<T> QuicClientBuilder<T> {
    /// 在优先使用IPv6的情况下，可以设置一个IPv4的地址，以备IPv6无法使用时的备用
    /// 必须bind的地址中一个是v4，一个是v6，才有意义
    pub fn enable_happy_eyeballs(&mut self) -> &mut Self {
        self.enable_happy_eyepballs = true;
        self
    }

    /// 是否高效复用连接，假如已经有了一个到server_name的连接，那再去连的话，就直接复用
    pub fn reuse_connection(&mut self) -> &mut Self {
        self.reuse_connection = true;
        self
    }

    /// 当服务端发来版本协商包，其中包含了支持的版本号，那么客户端可以选择使用哪个版本
    /// 将按照客户端设定的versions的顺序优先选择
    pub fn prefer_versions(&mut self, versions: impl IntoIterator<Item = u32>) -> &mut Self {
        self.preferred_versions.extend(versions);
        self
    }

    /// 设值客户端连接参数。若不设置，则会使用一组默认参数。
    /// 后续使用该QuicClient创建新连接，会直接使用这些参数。
    /// 可以多次调用该函数，覆盖上一次设置的参数。
    pub fn with_parameters(&mut self, parameters: impl Into<Parameters>) -> &mut Self {
        self.parameters = parameters.into();
        self
    }

    /// 设置客户端的证书，用于传输给服务端验证客户端身份
    /// 一般情况下，客户端都无需设置证书，只有特别的安全需求，才需要客户端提交证书
    /// 设置TokenRegisty的方法，当收到服务端的NewToken，客户端自行决定如何保存。
    /// 如不设置，则会丢弃这些NewToken
    /// TokenSink会在创建新连接时，尝试根据server_name获取可用Token
    /// TokenSink还需保存服务端颁发的关联Token，以便未来连接时使用
    pub fn with_token_sink(&mut self) -> &mut Self {
        todo!()
    }
}

impl QuicClientBuilder<TlsClientConfigBuilder<WantsVerifier>> {
    /// 验证服务端证书，是否正常的方法
    pub fn with_root_certificates(
        self,
        root_store: impl Into<Arc<rustls::RootCertStore>>,
    ) -> QuicClientBuilder<TlsClientConfigBuilder<WantsClientCert>> {
        QuicClientBuilder {
            addresses: self.addresses,
            reuse_connection: self.reuse_connection,
            enable_happy_eyepballs: self.enable_happy_eyepballs,
            preferred_versions: self.preferred_versions,
            parameters: self.parameters,
            tls_config: self.tls_config.with_root_certificates(root_store),
        }
    }
    pub fn with_webpki_verifier(
        self,
        verifier: Arc<rustls::client::WebPkiServerVerifier>,
    ) -> QuicClientBuilder<TlsClientConfigBuilder<WantsClientCert>> {
        QuicClientBuilder {
            addresses: self.addresses,
            reuse_connection: self.reuse_connection,
            enable_happy_eyepballs: self.enable_happy_eyepballs,
            preferred_versions: self.preferred_versions,
            parameters: self.parameters,
            tls_config: self.tls_config.with_webpki_verifier(verifier),
        }
    }
}

impl QuicClientBuilder<TlsClientConfigBuilder<WantsClientCert>> {
    pub fn with_cert(
        self,
        cert_file: impl AsRef<Path>,
        key_file: impl AsRef<Path>,
    ) -> QuicClientBuilder<TlsClientConfig> {
        let cert_chain = rustls_pemfile::certs(&mut BufReader::new(
            File::open(cert_file).expect("Failed to open cert file"),
        ))
        .collect::<Result<Vec<_>, _>>()
        .expect("Failed to read and extract cert from the cert file");

        let key_der = rustls_pemfile::private_key(&mut BufReader::new(
            File::open(key_file).expect("Failed to open private key file"),
        ))
        .expect("Failed to read PEM sections from the private key file")
        .unwrap();

        QuicClientBuilder {
            addresses: self.addresses,
            reuse_connection: self.reuse_connection,
            enable_happy_eyepballs: self.enable_happy_eyepballs,
            preferred_versions: self.preferred_versions,
            parameters: self.parameters,
            tls_config: self
                .tls_config
                .with_client_auth_cert(cert_chain, key_der)
                .expect("The private key was wrong encoded or failed validation"),
        }
    }

    pub fn without_cert(self) -> QuicClientBuilder<TlsClientConfig> {
        QuicClientBuilder {
            addresses: self.addresses,
            reuse_connection: self.reuse_connection,
            enable_happy_eyepballs: self.enable_happy_eyepballs,
            preferred_versions: self.preferred_versions,
            parameters: self.parameters,
            tls_config: self.tls_config.with_no_client_auth(),
        }
    }

    pub fn with_cert_resolver(
        self,
        cert_resolver: Arc<dyn rustls::client::ResolvesClientCert>,
    ) -> QuicClientBuilder<TlsClientConfig> {
        QuicClientBuilder {
            addresses: self.addresses,
            reuse_connection: self.reuse_connection,
            enable_happy_eyepballs: self.enable_happy_eyepballs,
            preferred_versions: self.preferred_versions,
            parameters: self.parameters,
            tls_config: self.tls_config.with_client_cert_resolver(cert_resolver),
        }
    }
}

impl QuicClientBuilder<TlsClientConfig> {
    pub fn build(self) -> QuicClient {
        // TODO: 创建好相应的usc
        QuicClient {
            addresses: self.addresses,
            _reuse_connection: self.reuse_connection,
            _enable_happy_eyepballs: self.enable_happy_eyepballs,
            _prefered_versions: self.preferred_versions,
            parameters: self.parameters,
            tls_config: Arc::new(self.tls_config),
        }
    }
}
