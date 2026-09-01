    # Attester-only public read (2026-08-16). Ubuntu nginx 1.18 has no njs.
    # Python filter on 127.0.0.1:18547 rejects writes/rope_*/txpool with HTTP 405
    # and proxies eth_* reads to local attester :8545. Matches BLUE routeAttesterRead.
    # Port 18547 (not 8547): Reth websocket already binds --ws.port 8547.
    location = /v1/read {
        proxy_pass http://127.0.0.1:18547;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header Connection "";
        proxy_set_header X-Rope-Internal-Token "";
        proxy_connect_timeout 2s;
        proxy_send_timeout 10s;
        proxy_read_timeout 10s;
        proxy_buffering off;
        add_header Access-Control-Allow-Origin * always;
        add_header Access-Control-Allow-Methods "GET, POST, OPTIONS" always;
        add_header Access-Control-Allow-Headers "Content-Type, Authorization" always;
        add_header X-Rope-Read-Pool attesters-only always;
    }
