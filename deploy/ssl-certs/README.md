# SSL Certificates

This directory contains the SSL certificates for production deployment.

## Structure

```
ssl-certs/
├── datachain.network/
│   ├── fullchain.pem      # Domain cert + CA chain
│   └── privkey.pem        # Private key
├── rope.network/
│   ├── fullchain.pem
│   └── privkey.pem
└── dcscan.io/
    ├── fullchain.pem
    └── privkey.pem
```

## ⚠️ Security Notice

- NEVER commit private keys to Git
- These files should be uploaded directly to the VPS
- Keep backups in a secure location (encrypted)

## Certificate Expiry

All certificates expire on **January 7, 2027**.

Set a reminder to renew before expiry!

