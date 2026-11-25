// Explicit certificate pairing for private-address data peers. No public PKI,
// proxy discovery, DNS lookup or silent plaintext downgrade participates.
package main

import (
	"context"
	"crypto/ed25519"
	"crypto/rand"
	"crypto/sha256"
	"crypto/tls"
	"crypto/x509"
	"crypto/x509/pkix"
	"encoding/hex"
	"encoding/pem"
	"errors"
	"io"
	"math/big"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"time"
)

type PeerConfig struct {
	Identity    string `json:"identity,omitempty"`
	Fingerprint string `json:"fingerprint,omitempty"`
	Listen      string `json:"listen,omitempty"`
	// ReadIdle bounds a paired client's wait for inbound data, in seconds
	// (peer_io.go): 0/omitted selects the 30 s default, explicit values are
	// 1..300. Plain loopback qualification clients refuse the field, and a
	// listener only validates it.
	ReadIdle int `json:"read_idle_seconds,omitempty"`
}

func initializeIdentity(configPath string) error {
	var cfg struct{}
	if err := strictControl(configPath, &cfg); err != nil {
		return err
	}
	dir := filepath.Join(filepath.Dir(configPath), "identity")
	if err := os.Mkdir(dir, 0700); err != nil {
		return errors.New("identity directory must be new")
	}
	if err := privateDirectory(dir); err != nil {
		return err
	}
	pub, key, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		return err
	}
	serialLimit := new(big.Int).Lsh(big.NewInt(1), 128)
	serial, err := rand.Int(rand.Reader, serialLimit)
	if err != nil {
		return err
	}
	template := &x509.Certificate{SerialNumber: serial, Subject: pkix.Name{CommonName: "Zerus artifact peer"},
		NotBefore: time.Now().Add(-5 * time.Minute), NotAfter: time.Now().Add(365 * 24 * time.Hour),
		KeyUsage:              x509.KeyUsageDigitalSignature | x509.KeyUsageCertSign,
		ExtKeyUsage:           []x509.ExtKeyUsage{x509.ExtKeyUsageClientAuth, x509.ExtKeyUsageServerAuth},
		BasicConstraintsValid: true, IsCA: true}
	der, err := x509.CreateCertificate(rand.Reader, template, template, pub, key)
	if err != nil {
		return err
	}
	keyDER, err := x509.MarshalPKCS8PrivateKey(key)
	if err != nil {
		return err
	}
	for name, block := range map[string]*pem.Block{"identity.crt": {Type: "CERTIFICATE", Bytes: der}, "identity.key": {Type: "PRIVATE KEY", Bytes: keyDER}} {
		f, err := os.OpenFile(filepath.Join(dir, name), os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
		if err != nil {
			return err
		}
		err = pem.Encode(f, block)
		syncErr, closeErr := f.Sync(), f.Close()
		if err != nil || syncErr != nil || closeErr != nil {
			return errors.New("identity persistence failed")
		}
	}
	hash := sha256.Sum256(der)
	return durableJSON(filepath.Join(dir, "public.json"), map[string]any{"protocol": deliveryProtocol, "fingerprint": hex.EncodeToString(hash[:])})
}

func loadIdentity(dir string) (tls.Certificate, string, error) {
	var pair tls.Certificate
	parents, _, err := holdDirectoryChain(dir)
	defer closeFiles(parents)
	if err != nil {
		return pair, "", err
	}
	read := func(name string) ([]byte, error) {
		f, err := openSealed(filepath.Join(dir, name), "", false)
		if err != nil {
			return nil, err
		}
		defer f.Close()
		b, err := io.ReadAll(io.LimitReader(f, maxControlBytes+1))
		if err != nil || len(b) > maxControlBytes {
			return nil, errors.New("identity exceeds size limit")
		}
		return b, nil
	}
	cert, err := read("identity.crt")
	if err != nil {
		return pair, "", err
	}
	key, err := read("identity.key")
	if err != nil {
		return pair, "", err
	}
	pair, err = tls.X509KeyPair(cert, key)
	for i := range key {
		key[i] = 0
	}
	if err != nil {
		return pair, "", errors.New("invalid peer identity")
	}
	if len(pair.Certificate) != 1 {
		return pair, "", errors.New("peer identity must contain one pinned certificate")
	}
	leaf, err := x509.ParseCertificate(pair.Certificate[0])
	if err != nil {
		return pair, "", err
	}
	if time.Now().Before(leaf.NotBefore) || time.Now().After(leaf.NotAfter) {
		return pair, "", errors.New("peer identity expired or not yet valid")
	}
	h := sha256.Sum256(pair.Certificate[0])
	return pair, hex.EncodeToString(h[:]), nil
}

func verifyPeer(pin string, usage x509.ExtKeyUsage) func(tls.ConnectionState) error {
	return func(state tls.ConnectionState) error {
		if !validHex(pin, 32) || len(state.PeerCertificates) != 1 {
			return errors.New("peer identity required")
		}
		leaf := state.PeerCertificates[0]
		h := sha256.Sum256(leaf.Raw)
		if hex.EncodeToString(h[:]) != pin {
			return errors.New("paired certificate identity changed")
		}
		roots := x509.NewCertPool()
		roots.AddCert(leaf)
		_, err := leaf.Verify(x509.VerifyOptions{Roots: roots, KeyUsages: []x509.ExtKeyUsage{usage}, CurrentTime: time.Now()})
		return err
	}
}

func privateAddress(address string, allowZero bool) error {
	host, port, err := net.SplitHostPort(address)
	if err != nil {
		return errors.New("peer requires a literal address and port")
	}
	ip := net.ParseIP(host)
	n, err := strconv.Atoi(port)
	if err != nil || ip == nil || ip.To4() == nil || (!ip.IsPrivate() && !ip.IsLoopback()) || ip.IsUnspecified() || n < 0 || (!allowZero && n == 0) || n > 65535 || strconv.Itoa(n) != port {
		return errors.New("peer requires a private IPv4 address and explicit port")
	}
	return nil
}

func validateEndpoint(endpoint, pin string) error {
	if pin == "" {
		client, err := fixedLoopbackClient(endpoint)
		if client != nil {
			client.CloseIdleConnections()
		}
		return err
	}
	if !validHex(pin, 32) {
		return errors.New("invalid certificate fingerprint")
	}
	u, err := url.Parse(endpoint)
	if err != nil || u.Scheme != "https" || u.User != nil || u.Path != "" || u.RawQuery != "" || u.ForceQuery || u.Fragment != "" || u.Opaque != "" {
		return errors.New("private peer requires HTTPS without URL routing fields")
	}
	return privateAddress(u.Host, false)
}

func dataClient(endpoint, pin string, cfg PeerConfig) (*http.Client, error) {
	idle, err := readIdleBound(cfg.ReadIdle)
	if err != nil {
		return nil, err
	}
	if cfg.Identity == "" && cfg.Fingerprint == "" && pin == "" {
		if cfg.ReadIdle != 0 {
			return nil, errors.New("read_idle_seconds applies only to paired peers")
		}
		return fixedLoopbackClient(endpoint)
	}
	if pin == "" || pin != cfg.Fingerprint || cfg.Identity == "" {
		return nil, errors.New("paired identity is required; plaintext downgrade refused")
	}
	if err := validateEndpoint(endpoint, pin); err != nil {
		return nil, err
	}
	pair, _, err := loadIdentity(cfg.Identity)
	if err != nil {
		return nil, err
	}
	u, _ := url.Parse(endpoint)
	address := u.Host
	tlsConfig := &tls.Config{MinVersion: tls.VersionTLS13, Certificates: []tls.Certificate{pair}, InsecureSkipVerify: true,
		VerifyConnection: verifyPeer(pin, x509.ExtKeyUsageServerAuth), SessionTicketsDisabled: true}
	transport := &http.Transport{Proxy: nil, DisableCompression: true, MaxConnsPerHost: 2, MaxIdleConns: 2, MaxIdleConnsPerHost: 2,
		IdleConnTimeout: 5 * time.Second, MaxResponseHeaderBytes: 4096, TLSHandshakeTimeout: 5 * time.Second, TLSClientConfig: tlsConfig,
		DialContext: func(ctx context.Context, _, target string) (net.Conn, error) {
			if target != address {
				return nil, errors.New("alternate peer refused")
			}
			conn, err := (&net.Dialer{Timeout: 5 * time.Second}).DialContext(ctx, "tcp4", address)
			if err != nil {
				return nil, err
			}
			// Below TLS, so handshake and response reads share the bound.
			return newIdleConn(conn, idle), nil
		}}
	return &http.Client{Transport: transport, Timeout: 15 * time.Minute, CheckRedirect: func(_ *http.Request, _ []*http.Request) error { return errors.New("redirect refused") }}, nil
}

func dataListener(cfg PeerConfig) (net.Listener, string, string, error) {
	if cfg.Identity == "" && cfg.Fingerprint == "" && cfg.Listen == "" {
		if cfg.ReadIdle != 0 {
			return nil, "", "", errors.New("read_idle_seconds applies only to paired peers")
		}
		listener, err := net.ListenTCP("tcp4", &net.TCPAddr{IP: net.IPv4(127, 0, 0, 1)})
		if err != nil {
			return nil, "", "", err
		}
		return listener, "http://" + listener.Addr().String(), "", nil
	}
	if cfg.Identity == "" || !validHex(cfg.Fingerprint, 32) {
		return nil, "", "", errors.New("paired client identity is required")
	}
	if _, err := readIdleBound(cfg.ReadIdle); err != nil {
		return nil, "", "", err
	}
	address := cfg.Listen
	if address == "" {
		address = "127.0.0.1:0"
	}
	if err := privateAddress(address, true); err != nil {
		return nil, "", "", err
	}
	pair, pin, err := loadIdentity(cfg.Identity)
	if err != nil {
		return nil, "", "", err
	}
	listener, err := net.Listen("tcp4", address)
	if err != nil {
		return nil, "", "", err
	}
	config := &tls.Config{MinVersion: tls.VersionTLS13, Certificates: []tls.Certificate{pair}, ClientAuth: tls.RequireAnyClientCert,
		VerifyConnection: verifyPeer(cfg.Fingerprint, x509.ExtKeyUsageClientAuth), SessionTicketsDisabled: true}
	return tls.NewListener(listener, config), "https://" + listener.Addr().String(), pin, nil
}
