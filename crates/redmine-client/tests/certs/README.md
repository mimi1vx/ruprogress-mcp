# TLS test fixtures

Two throwaway, self-signed certificates used by `tests/tls.rs` to assert that
a **well-formed** PEM is accepted by the client builder (the interesting cases
in that file are the malformed ones, which are inline byte literals). Nothing
here authenticates anything: no TLS handshake is performed by the test suite,
the certificates are never presented to a real server, and no hostname or SAN
is set.

| File | Contents | Subject |
|---|---|---|
| `ca.pem` | certificate only | `CN=Test CA` |
| `client_identity.pem` | certificate + P-256 private key | `CN=Test Client` |

`client_identity.pem` contains a private key, which is why the repository's
blanket `*.pem` ignore rule exists in the first place. This directory is the
one documented exception to it. **Do not put anything else here**, and do not
widen the exception: the rule is what stops a real key from being committed by
accident.

Both expire **2036-08-02**. If a future CI run fails on an expired fixture
rather than on the behaviour under test, regenerate:

```sh
openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout /dev/null -out ca.pem -days 3650 -subj '/CN=Test CA'

openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -nodes \
  -keyout key.tmp -out cert.tmp -days 3650 -subj '/CN=Test Client'
cat cert.tmp key.tmp > client_identity.pem && rm cert.tmp key.tmp
```
