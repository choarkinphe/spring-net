# Public test certificates

`ca.der`, `server.der`, and `server-key.der` are intentionally public, static test fixtures for isolated loopback TLS tests. The server certificate identifies `diagnostic.test` and loopback IPs; it is not a production identity.

The private key is not a secret and must never be used for a real service. These files are referenced only by test code. Production checks retain normal certificate and hostname verification using bundled Mozilla roots; they do not trust this test CA.
