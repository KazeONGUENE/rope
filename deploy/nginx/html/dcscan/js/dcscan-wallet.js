/*!
 * dcscan-wallet.js — DCScan Connect Wallet button + modal.
 *
 * Ported from tanastok.io's WalletConnectModal (tanastok-app
 * src/app/components/auth/WalletConnectModal.tsx) and restyled with the
 * Datachain Rope canonical design system (dcscan.io / datachain.network):
 * light theme, Inter, black primary buttons, pastel semantic pills.
 *
 * Behavior:
 *  - Takes over every header "Connect" button (.header-actions .btn-secondary,
 *    header .btn-secondary, .connect-btn) on the page.
 *  - Opens a right-side full-height drawer (slide-in 0.3s, blurred overlay)
 *    with a Datawallet+ section and an "Other Wallets" list
 *    (MetaMask, XDC Pay, Coinbase Wallet, Phantom, Exodus, Trust Wallet)
 *    with live Detected / Recent status pills and per-row connect spinners.
 *  - EVM wallets are switched/added to Datachain Rope (chainId 271828).
 *  - Connected state: the header button becomes an address pill with a
 *    dropdown (copy address, view on DCScan, disconnect), persisted in
 *    localStorage and kept in sync with accountsChanged events.
 */
(function () {
  'use strict';

  /* ------------------------------------------------------------ config */
  var ROPE_CHAIN = {
    chainId: '0x425d4', // 271828
    chainName: 'Datachain Rope',
    nativeCurrency: { name: 'DC FAT', symbol: 'FAT', decimals: 18 },
    rpcUrls: ['https://erpc.datachain.network'],
    blockExplorerUrls: ['https://dcscan.io']
  };
  /* Per-site overrides — set window.DCW_CONFIG BEFORE loading this script:
   *   { brand: 'Datachain Network', assetBase: '/assets/wallets/',
   *     explorerBase: 'https://dcscan.io' }
   * Defaults keep the original dcscan.io behavior. */
  var SITE = window.DCW_CONFIG || {};
  var BRAND = SITE.brand || 'DCScan';
  var ASSET_BASE = SITE.assetBase || '/assets/wallets/';
  var EXPLORER_BASE = SITE.explorerBase || '';
  var LS_ADDR = 'dcscan_wallet_addr';
  var LS_NAME = 'dcscan_wallet_name';
  /* Datachain ID — ecosystem identity gateway (Datawallet+ SSO).
   * Ed25519-signed ecosystem tokens; JWKS at /.well-known/jwks.json. */
  var IDP_URL = 'https://id.datachain.network';
  var LS_TOKEN = 'dcscan_id_token';
  var LS_EMAIL = 'dcscan_id_email';

  /* ------------------------------------------------------------ styles */
  var CSS = [
    '.dcw-overlay{position:fixed;inset:0;z-index:9000;background:rgba(0,0,0,.5);-webkit-backdrop-filter:blur(4px);backdrop-filter:blur(4px);opacity:0;transition:opacity .2s ease}',
    '.dcw-overlay.dcw-open{opacity:1}',
    '.dcw-drawer{position:fixed;top:0;right:0;bottom:auto;left:auto;height:100vh;width:100%;max-width:512px;z-index:9001;background:#ffffff;border-left:1px solid #e4e4e7;box-shadow:0 25px 50px -12px rgba(0,0,0,.25);overflow-y:auto;transform:translateX(100%);opacity:0;font-family:\'Inter\',-apple-system,BlinkMacSystemFont,sans-serif;color:#18181b}',
    '.dcw-drawer.dcw-open{animation:dcwSlideInRight .3s ease-out forwards}',
    '.dcw-drawer.dcw-closing{animation:dcwSlideOutRight .25s ease-in forwards}',
    '@keyframes dcwSlideInRight{from{transform:translateX(100%);opacity:0}to{transform:translateX(0);opacity:1}}',
    '@keyframes dcwSlideOutRight{from{transform:translateX(0);opacity:1}to{transform:translateX(100%);opacity:0}}',
    '.dcw-inner{padding:1.5rem}',
    '.dcw-close{position:absolute;right:1rem;top:1rem;background:none;border:none;cursor:pointer;color:#71717a;opacity:.7;padding:4px;border-radius:6px;transition:opacity .15s}',
    '.dcw-close:hover{opacity:1;background:#f4f4f5}',
    '.dcw-title{font-size:1.25rem;font-weight:600;text-align:center;color:#18181b;margin:0 0 1rem;line-height:1.4}',
    '.dcw-section-title{font-size:.95rem;font-weight:600;color:#3f3f46;margin:1.5rem 0 .75rem}',
    '.dcw-row{display:flex;align-items:center;width:100%;gap:.75rem;padding:.75rem;background:#ffffff;border:1px solid #d4d4d8;border-radius:8px;cursor:pointer;text-align:left;font-family:inherit;font-size:.875rem;transition:background .15s;margin-bottom:.5rem}',
    '.dcw-row:hover{background:#fafafa}',
    '.dcw-row:disabled{opacity:.55;cursor:not-allowed}',
    '.dcw-row img{width:28px;height:28px;object-fit:contain;border-radius:6px;flex-shrink:0}',
    '.dcw-row .dcw-wname{font-weight:500;color:#18181b;flex:1}',
    '.dcw-pill{font-size:.7rem;font-weight:600;padding:2px 8px;border-radius:20px;white-space:nowrap}',
    '.dcw-pill.detected{background:#dcfce7;color:#166534}',
    '.dcw-pill.recent{background:#dbeafe;color:#1e40af}',
    '.dcw-pill.none{background:#f4f4f5;color:#71717a}',
    '.dcw-spinner{width:16px;height:16px;border:2px solid #3b82f6;border-top-color:transparent;border-radius:50%;animation:dcwSpin .7s linear infinite;flex-shrink:0}',
    '@keyframes dcwSpin{to{transform:rotate(360deg)}}',
    '.dcw-dw{display:flex;align-items:center;width:100%;gap:1rem;padding:1rem;background:#fafafa;border:1px solid #d4d4d8;border-radius:8px;cursor:pointer;text-align:left;font-family:inherit;transition:background .15s}',
    '.dcw-dw:hover{background:#f4f4f5}',
    '.dcw-dw img{width:40px;height:40px;object-fit:contain;border-radius:8px;flex-shrink:0}',
    '.dcw-dw .dcw-dw-t{font-size:.95rem;font-weight:600;color:#18181b;margin:0}',
    '.dcw-dw .dcw-dw-s{font-size:.75rem;color:#71717a;margin:2px 0 0}',
    '.dcw-chev{margin-left:auto;color:#71717a;transition:transform .2s;flex-shrink:0}',
    '.dcw-chev.dcw-rot{transform:rotate(180deg)}',
    '.dcw-dw-body{overflow:hidden;max-height:0;opacity:0;transition:max-height .3s ease-in-out,opacity .3s ease-in-out}',
    '.dcw-dw-body.dcw-openb{max-height:900px;opacity:1;margin-top:.5rem}',
    '.dcw-signin{border:1px solid #d4d4d8;background:#ffffff;border-radius:8px;padding:.75rem;margin-bottom:.5rem}',
    '.dcw-signin .dcw-si-t{font-size:.875rem;font-weight:600;color:#18181b;margin:0 0 .625rem}',
    '.dcw-signin label{display:block;font-size:.7rem;font-weight:600;color:#52525b;margin:0 0 .25rem}',
    '.dcw-signin input{display:block;width:100%;box-sizing:border-box;padding:.5rem .625rem;margin-bottom:.625rem;background:#fafafa;border:1px solid #d4d4d8;border-radius:6px;font-family:inherit;font-size:.8125rem;color:#18181b;outline:none;transition:border-color .15s}',
    '.dcw-signin input:focus{border-color:#18181b;background:#ffffff}',
    '.dcw-si-submit{display:flex;align-items:center;justify-content:center;gap:.5rem;width:100%;padding:.5625rem .875rem;background:#18181b;border:none;border-radius:8px;font-family:inherit;font-size:.8125rem;font-weight:600;color:#ffffff;cursor:pointer;transition:background .15s}',
    '.dcw-si-submit:hover{background:#3f3f46}',
    '.dcw-si-submit:disabled{opacity:.6;cursor:not-allowed}',
    '.dcw-si-divider{display:flex;align-items:center;gap:.625rem;margin:.75rem 0;font-size:.7rem;color:#a1a1aa}',
    '.dcw-si-divider::before,.dcw-si-divider::after{content:"";flex:1;height:1px;background:#e4e4e7}',
    '.dcw-appbox{border:1px solid #d4d4d8;background:#ffffff;border-radius:8px;padding:.75rem;margin-bottom:.5rem}',
    '.dcw-appbox .dcw-app-h{display:flex;align-items:flex-start;gap:.75rem}',
    '.dcw-appbox .dcw-app-t{font-size:.875rem;font-weight:600;color:#18181b;margin:0}',
    '.dcw-appbox .dcw-app-s{font-size:.75rem;color:#71717a;margin:2px 0 0}',
    '.dcw-badges{display:flex;flex-wrap:wrap;align-items:center;justify-content:center;gap:.75rem;margin-top:.75rem}',
    '.dcw-badges a{display:flex;height:48px;align-items:center;justify-content:center;overflow:hidden;border-radius:6px;transition:opacity .15s}',
    '.dcw-badges a:hover{opacity:.9}',
    '.dcw-badges img{display:block;height:100%;width:auto;object-fit:contain}',
    '.dcw-coming{font-size:.75rem;color:#71717a;text-align:center;margin:.75rem 0 0}',
    '.dcw-feedback{font-size:.75rem;padding:.625rem;border-radius:6px;margin-top:1rem;line-height:1.5}',
    '.dcw-feedback.ok{background:#dcfce7;color:#166534}',
    '.dcw-feedback.err{background:#fee2e2;color:#991b1b}',
    '.dcw-mention{font-size:.75rem;color:#71717a;margin-top:1.5rem;line-height:1.6}',
    '.dcw-mention a{color:#1e40af;text-decoration:none}',
    '.dcw-mention a:hover{text-decoration:underline}',
    '.dcw-cancel{display:block;width:100%;margin-top:1rem;padding:.5rem .875rem;background:none;border:none;border-radius:8px;font-family:inherit;font-size:.875rem;font-weight:500;color:#52525b;cursor:pointer;text-align:center;transition:background .15s}',
    '.dcw-cancel:hover{background:#f4f4f5;color:#18181b}',
    /* connected-state pill + dropdown on the header button */
    '.dcw-connected{position:relative}',
    '.dcw-addr{font-family:\'JetBrains Mono\',ui-monospace,Menlo,monospace;font-size:12px}',
    '.dcw-menu{position:absolute;right:0;top:calc(100% + 6px);z-index:8999;min-width:210px;background:#ffffff;border:1px solid #e4e4e7;border-radius:8px;box-shadow:0 10px 15px -3px rgba(0,0,0,.1);padding:.375rem;display:none}',
    '.dcw-menu.dcw-open{display:block}',
    '.dcw-menu button,.dcw-menu a{display:flex;align-items:center;gap:.5rem;width:100%;padding:.5rem .625rem;background:none;border:none;border-radius:6px;font-family:inherit;font-size:.8125rem;font-weight:500;color:#3f3f46;cursor:pointer;text-align:left;text-decoration:none;transition:background .15s}',
    '.dcw-menu button:hover,.dcw-menu a:hover{background:#f4f4f5;color:#18181b}',
    '.dcw-menu .dcw-menu-danger{color:#991b1b}',
    '.dcw-menu .dcw-menu-danger:hover{background:#fee2e2}',
    '.dcw-menu .dcw-menu-head{padding:.5rem .625rem;font-size:.7rem;color:#71717a;border-bottom:1px solid #f4f4f5;margin-bottom:.25rem}',
    '@media(max-width:560px){.dcw-drawer{max-width:100%}}'
  ].join('\n');

  /* ------------------------------------------------------------ svg bits */
  var SVG_X = '<svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 6 6 18"/><path d="m6 6 12 12"/></svg>';
  var SVG_CHEV = '<svg width="20" height="20" viewBox="0 0 20 20" fill="none"><path d="M5 7.5L10 12.5L15 7.5" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/></svg>';
  var SVG_PHONE = '<svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="#3f3f46" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round"><rect width="14" height="20" x="5" y="2" rx="2" ry="2"/><path d="M12 18h.01"/></svg>';
  var SVG_WALLET = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" style="vertical-align:-2px;margin-right:6px"><path d="M21 12V7H5a2 2 0 0 1 0-4h14v4"/><path d="M3 7v10a2 2 0 0 0 2 2h16v-5"/><path d="M7 12h4"/></svg>';
  var SVG_CHECK = '<svg width="13" height="13" viewBox="0 0 20 20" fill="#166534" style="vertical-align:-2px;margin-right:6px"><path fill-rule="evenodd" d="M16.707 5.293a1 1 0 010 1.414l-8 8a1 1 0 01-1.414 0l-4-4a1 1 0 011.414-1.414L8 12.586l7.293-7.293a1 1 0 011.414 0z" clip-rule="evenodd"/></svg>';
  var SVG_COPY = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><rect width="14" height="14" x="8" y="8" rx="2" ry="2"/><path d="M4 16c-1.1 0-2-.9-2-2V4c0-1.1.9-2 2-2h10c1.1 0 2 .9 2 2"/></svg>';
  var SVG_EXT = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M15 3h6v6"/><path d="M10 14 21 3"/><path d="M18 13v6a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2h6"/></svg>';
  var SVG_OUT = '<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M9 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h4"/><polyline points="16 17 21 12 16 7"/><line x1="21" x2="9" y1="12" y2="12"/></svg>';

  /* ------------------------------------------------------------ providers */
  function ethProviders() {
    var eth = window.ethereum;
    if (!eth) return [];
    return Array.isArray(eth.providers) && eth.providers.length ? eth.providers : [eth];
  }
  function findEth(pred) {
    var list = ethProviders();
    for (var i = 0; i < list.length; i++) if (pred(list[i])) return list[i];
    return null;
  }
  function getMetaMask()  { return findEth(function (p) { return p.isMetaMask && !p.isXDCPay && !p.isTrust && !p.isCoinbaseWallet; }) || findEth(function (p) { return p.isMetaMask; }); }
  function getXdcPay()    { return (window.xdc) || findEth(function (p) { return p.isXDCPay || p.isXdc; }); }
  function getCoinbase()  { return (window.coinbaseWalletExtension) || findEth(function (p) { return p.isCoinbaseWallet; }); }
  function getPhantom()   { return (window.solana && window.solana.isPhantom) ? window.solana : null; }
  function getExodus()    { return (window.exodus && window.exodus.ethereum && window.exodus.ethereum.isExodus) ? window.exodus.ethereum : null; }
  function getTrust()     { return (window.trustwallet) || findEth(function (p) { return p.isTrust || p.isTrustWallet; }); }

  /* ------------------------------------------------------------ chain */
  function ensureRopeChain(provider) {
    return provider.request({
      method: 'wallet_switchEthereumChain',
      params: [{ chainId: ROPE_CHAIN.chainId }]
    }).catch(function (err) {
      var msg = (err && err.message) || '';
      if ((err && err.code === 4902) || /Unrecognized chain ID|wallet_addEthereumChain/i.test(msg)) {
        return provider.request({ method: 'wallet_addEthereumChain', params: [ROPE_CHAIN] })
          .then(function () {
            return provider.request({
              method: 'wallet_switchEthereumChain',
              params: [{ chainId: ROPE_CHAIN.chainId }]
            });
          });
      }
      if (err && err.code === 4001) return null; // user declined switch — connection still valid
      return null; // chain switch is best-effort; never fail the connect
    });
  }

  /* Push the Datachain Rope network configuration (chainId 271828, RPC,
   * FAT currency, dcscan.io explorer) into every injected EVM provider —
   * MetaMask, Coinbase, Trust, XDC Pay, Exodus. Best-effort: providers
   * that already know the chain resolve instantly; user rejections are
   * swallowed. Resolves with the number of providers that accepted. */
  function syncRopeNetworkToWallets() {
    var providers = ethProviders();
    if (!providers.length) return Promise.resolve(0);
    var added = 0;
    var chain = providers.reduce(function (p, provider) {
      return p.then(function () {
        return provider
          .request({ method: 'wallet_addEthereumChain', params: [ROPE_CHAIN] })
          .then(function () { added++; })
          .catch(function () { /* rejected or unsupported — ignore */ });
      });
    }, Promise.resolve());
    return chain.then(function () { return added; });
  }

  /* ------------------------------------------------------------ state */
  var state = {
    address: localStorage.getItem(LS_ADDR) || '',
    walletName: localStorage.getItem(LS_NAME) || '',
    email: localStorage.getItem(LS_EMAIL) || '',
    connecting: null,
    drawer: null,
    overlay: null,
    buttons: []
  };

  function short(addr) {
    if (!addr) return '';
    return addr.length > 12 ? addr.slice(0, 6) + '...' + addr.slice(-4) : addr;
  }
  function esc(s) {
    return String(s).replace(/[<>&"]/g, function (c) {
      return { '<': '&lt;', '>': '&gt;', '&': '&amp;', '"': '&quot;' }[c];
    });
  }

  /* Notify host pages of connection-state transitions so they can run
   * their own post-connect flows (e.g. the EDC console exchanges the
   * connection for an EIP-191 server session). Fired on window as
   * `datachain-wallet:connected` / `datachain-wallet:disconnected`. */
  function emitConnectionEvent(connected) {
    try {
      window.dispatchEvent(new CustomEvent(
        connected ? 'datachain-wallet:connected' : 'datachain-wallet:disconnected',
        { detail: { address: state.address, walletName: state.walletName, email: state.email } }
      ));
    } catch (e) { /* CustomEvent unavailable — nothing to notify */ }
  }

  function persist(addr, name) {
    var wasConnected = !!(state.address || (state.walletName === 'Datawallet+' && state.email));
    state.address = addr || '';
    state.walletName = name || '';
    if (addr) {
      localStorage.setItem(LS_ADDR, addr);
      localStorage.setItem(LS_NAME, name || '');
    } else {
      localStorage.removeItem(LS_ADDR);
      localStorage.removeItem(LS_NAME);
    }
    if (name !== 'Datawallet+') {
      state.email = '';
      localStorage.removeItem(LS_TOKEN);
      localStorage.removeItem(LS_EMAIL);
    }
    renderButtons();
    var isConnected = !!(state.address || (state.walletName === 'Datawallet+' && state.email));
    if (isConnected) emitConnectionEvent(true);
    else if (wasConnected) emitConnectionEvent(false);
  }

  /* Persist a Datawallet+ (Datachain ID) session: ecosystem token +
   * email + primary on-chain address (may be empty when the account
   * has no wallet bound yet). */
  function persistIdentity(token, email, addr) {
    state.email = email || '';
    localStorage.setItem(LS_TOKEN, token);
    localStorage.setItem(LS_EMAIL, state.email);
    state.address = addr || '';
    state.walletName = 'Datawallet+';
    localStorage.setItem(LS_ADDR, state.address);
    localStorage.setItem(LS_NAME, 'Datawallet+');
    renderButtons();
    emitConnectionEvent(true);
  }

  /* Decode the exp claim of a compact JWT without verifying — the
   * gateway verifies server-side; this is only a local session check. */
  function tokenExpired(token) {
    try {
      var payload = JSON.parse(atob(token.split('.')[1].replace(/-/g, '+').replace(/_/g, '/')));
      return !payload.exp || payload.exp * 1000 <= Date.now();
    } catch (e) {
      return true;
    }
  }

  /* ------------------------------------------------------------ wallets */
  function walletList() {
    var lastName = localStorage.getItem(LS_NAME);
    function status(detected, name) {
      if (detected && lastName === name) return 'Recent';
      if (detected) return 'Detected';
      return '-';
    }
    return [
      { id: 'metamask', name: 'MetaMask',        logo: ASSET_BASE + 'metamask.svg',    detected: !!getMetaMask(), get status() { return status(this.detected, this.name); }, connect: connectMetaMask },
      { id: 'xdcpay',   name: 'XDC Pay',         logo: ASSET_BASE + 'xdc.jpeg',        detected: !!getXdcPay(),   get status() { return status(this.detected, this.name); }, connect: connectXdcPay },
      { id: 'coinbase', name: 'Coinbase Wallet', logo: ASSET_BASE + 'coinbase.svg',    detected: !!getCoinbase(), get status() { return status(this.detected, this.name); }, connect: connectCoinbase },
      { id: 'phantom',  name: 'Phantom Wallet',  logo: ASSET_BASE + 'phandom.png',     detected: !!getPhantom(),  get status() { return status(this.detected, this.name); }, connect: connectPhantom },
      { id: 'exodus',   name: 'Exodus',          logo: ASSET_BASE + 'exodus.png',      detected: !!getExodus(),   get status() { return status(this.detected, this.name); }, connect: connectExodus },
      { id: 'trust',    name: 'Trust Wallet',    logo: ASSET_BASE + 'trustwallet.svg', detected: !!getTrust(),    get status() { return status(this.detected, this.name); }, connect: connectTrust }
    ];
  }

  function evmConnect(provider, walletName, notDetectedMsg) {
    if (!provider) return Promise.reject(new Error(notDetectedMsg));
    return provider.request({ method: 'eth_requestAccounts' }).then(function (accounts) {
      var addr = accounts && accounts[0];
      if (!addr) throw new Error('No accounts returned from ' + walletName);
      return ensureRopeChain(provider).then(function () { return addr; });
    });
  }
  function connectMetaMask() { return evmConnect(getMetaMask(), 'MetaMask', 'MetaMask not detected. Please install it.'); }
  function connectXdcPay()   { return evmConnect(getXdcPay(), 'XDC Pay', 'XDC Pay not detected. Please install it.'); }
  function connectCoinbase() { return evmConnect(getCoinbase(), 'Coinbase Wallet', 'Coinbase Wallet not detected. Please install it.'); }
  function connectExodus()   { return evmConnect(getExodus(), 'Exodus', 'Exodus (Ethereum) not detected. Please install and enable it.'); }
  function connectTrust()    { return evmConnect(getTrust(), 'Trust Wallet', 'Trust Wallet not detected. Please install it.'); }
  function connectPhantom() {
    var provider = getPhantom();
    if (!provider) return Promise.reject(new Error('Phantom Wallet not found. Please install it.'));
    return provider.connect({ onlyIfTrusted: false }).then(function (resp) {
      return resp.publicKey.toString(); // Solana wallet: no EVM chain switch
    });
  }

  /* ------------------------------------------------------------ modal */
  function buildDrawer() {
    var overlay = document.createElement('div');
    overlay.className = 'dcw-overlay';

    var drawer = document.createElement('div');
    drawer.className = 'dcw-drawer';
    drawer.setAttribute('role', 'dialog');
    drawer.setAttribute('aria-modal', 'true');
    drawer.setAttribute('aria-label', 'Connect your Wallet');

    var rows = walletList().map(function (w) {
      var pillClass = w.status === 'Detected' ? 'detected' : w.status === 'Recent' ? 'recent' : 'none';
      return '<button class="dcw-row" data-dcw-wallet="' + w.id + '">' +
        '<img src="' + w.logo + '" alt="' + esc(w.name) + '">' +
        '<span class="dcw-wname">' + esc(w.name) + '</span>' +
        '<span class="dcw-pill ' + pillClass + '">' + esc(w.status) + '</span>' +
        '<span class="dcw-rowspin" style="display:none"><span class="dcw-spinner"></span></span>' +
        '</button>';
    }).join('');

    drawer.innerHTML =
      '<div class="dcw-inner">' +
        '<button class="dcw-close" aria-label="Close">' + SVG_X + '</button>' +
        '<h2 class="dcw-title">Connect your Wallet to ' + esc(BRAND) + '</h2>' +

        /* DATAWALLET+ collapsible section */
        '<button class="dcw-dw" data-dcw-toggle="dw">' +
          '<img src="' + ASSET_BASE + 'datawallet-plus.png" alt="Datawallet+ Logo">' +
          '<span style="flex:1;min-width:0">' +
            '<span class="dcw-dw-t" style="display:block">Connect With DATAWALLET+</span>' +
            '<span class="dcw-dw-s" style="display:block">Sign in with your Datawallet+ credentials</span>' +
          '</span>' +
          '<span class="dcw-chev">' + SVG_CHEV + '</span>' +
        '</button>' +
        '<div class="dcw-dw-body" data-dcw-body="dw">' +
          '<form class="dcw-signin" data-dcw-form="dw">' +
            '<p class="dcw-si-t">Sign in with Datawallet+</p>' +
            '<label for="dcw-si-email">Email</label>' +
            '<input id="dcw-si-email" type="email" name="email" autocomplete="email" placeholder="you@example.com" required>' +
            '<label for="dcw-si-password">Password</label>' +
            '<input id="dcw-si-password" type="password" name="password" autocomplete="current-password" placeholder="Your Datawallet+ password" required>' +
            '<button type="submit" class="dcw-si-submit">' +
              '<span class="dcw-si-label">Sign In</span>' +
              '<span class="dcw-si-spin" style="display:none"><span class="dcw-spinner" style="border-color:#ffffff;border-top-color:transparent"></span></span>' +
            '</button>' +
          '</form>' +
          '<div class="dcw-si-divider">or get the app</div>' +
          '<div class="dcw-appbox">' +
            '<div class="dcw-app-h">' + SVG_PHONE +
              '<div style="min-width:0;flex:1">' +
                '<p class="dcw-app-t">Get Datawallet+ App</p>' +
                '<p class="dcw-app-s">Install the mobile app from Google Play or the App Store.</p>' +
              '</div>' +
            '</div>' +
            '<div class="dcw-badges">' +
              '<a href="https://play.google.com/store/apps/details?id=com.datawallet.plus" target="_blank" rel="noopener noreferrer">' +
                '<img src="' + ASSET_BASE + 'google-play-badge.svg" alt="Get it on Google Play">' +
              '</a>' +
              '<a href="https://apps.apple.com/fr/app/datawallet/id6448479741" target="_blank" rel="noopener noreferrer">' +
                '<img src="' + ASSET_BASE + 'app-store-badge.svg" alt="Download on the App Store">' +
              '</a>' +
            '</div>' +
            '<p class="dcw-coming">Chrome extension — coming soon</p>' +
          '</div>' +
        '</div>' +

        /* Other Wallets */
        '<h3 class="dcw-section-title">Other Wallets</h3>' +
        '<div class="dcw-list">' + rows + '</div>' +

        '<div class="dcw-feedback" style="display:none"></div>' +

        '<p class="dcw-mention"><strong>Mention*</strong><br>' +
          'By connecting a wallet, you agree to ' + esc(BRAND) + ' by Datachain Foundation\u2019s Terms of Service and consent to its Privacy Policy. ' +
          'Don\u2019t have a wallet? <a href="https://datawallet.plus" target="_blank" rel="noopener noreferrer">Get Datawallet+</a>.' +
        '</p>' +

        '<button class="dcw-cancel">Cancel</button>' +
      '</div>';

    document.body.appendChild(overlay);
    document.body.appendChild(drawer);
    state.overlay = overlay;
    state.drawer = drawer;

    overlay.addEventListener('click', closeDrawer);
    drawer.querySelector('.dcw-close').addEventListener('click', closeDrawer);
    drawer.querySelector('.dcw-cancel').addEventListener('click', closeDrawer);
    document.addEventListener('keydown', function (e) {
      if (e.key === 'Escape' && drawer.classList.contains('dcw-open')) closeDrawer();
    });

    drawer.querySelector('[data-dcw-toggle="dw"]').addEventListener('click', function () {
      var body = drawer.querySelector('[data-dcw-body="dw"]');
      var chev = this.querySelector('.dcw-chev');
      body.classList.toggle('dcw-openb');
      chev.classList.toggle('dcw-rot');
    });

    drawer.querySelectorAll('[data-dcw-wallet]').forEach(function (btn) {
      btn.addEventListener('click', function () { pickWallet(btn.getAttribute('data-dcw-wallet')); });
    });

    drawer.querySelector('[data-dcw-form="dw"]').addEventListener('submit', function (e) {
      e.preventDefault();
      datawalletSignIn(this);
    });
  }

  /* --------------------------------------------- Datawallet+ sign-in */
  function datawalletSignIn(form) {
    if (state.connecting) return;
    var email = form.querySelector('#dcw-si-email').value.trim();
    var password = form.querySelector('#dcw-si-password').value;
    if (!email || !password) return;

    var submit = form.querySelector('.dcw-si-submit');
    var label = form.querySelector('.dcw-si-label');
    var spin = form.querySelector('.dcw-si-spin');
    state.connecting = 'Datawallet+';
    submit.disabled = true;
    label.textContent = 'Signing in\u2026';
    spin.style.display = '';
    refreshRows();
    feedback('Verifying your Datawallet+ credentials\u2026');

    fetch(IDP_URL + '/v1/auth/login', {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ email: email, password: password })
    }).then(function (res) {
      return res.json().then(function (data) { return { ok: res.ok, data: data }; });
    }).then(function (result) {
      if (!result.ok || !result.data.token) {
        var code = result.data && result.data.error;
        var msg = code === 'invalid_credentials' ? 'Email or password is incorrect.'
          : code === 'rate_limited' ? 'Too many attempts \u2014 please wait a few minutes.'
          : (result.data && result.data.message) || 'Sign-in failed. Please try again.';
        throw new Error(msg);
      }
      var user = result.data.user || {};
      persistIdentity(result.data.token, user.email || email, user.primary_address || '');
      form.querySelector('#dcw-si-password').value = '';
      feedback('Connected with Datawallet+! Datachain Rope (chainId 271828) is your primary network in Datawallet+.');
      /* Mirror the Rope network configuration into any injected browser
       * wallets (MetaMask first and foremost) so the user's whole wallet
       * stack knows chainId 271828 / erpc.datachain.network / FAT. */
      syncRopeNetworkToWallets().then(function (added) {
        if (added > 0) {
          feedback('Connected with Datawallet+! Datachain Rope network also added to ' + added + ' browser wallet' + (added > 1 ? 's' : '') + ' (MetaMask & co).');
        }
      });
      setTimeout(function () {
        state.connecting = null;
        submit.disabled = false;
        label.textContent = 'Sign In';
        spin.style.display = 'none';
        refreshRows();
        closeDrawer();
      }, 1600);
    }).catch(function (err) {
      feedback('Datawallet+: ' + ((err && err.message) || 'Sign-in failed'));
      state.connecting = null;
      submit.disabled = false;
      label.textContent = 'Sign In';
      spin.style.display = 'none';
      refreshRows();
    });
  }

  function refreshRows() {
    if (!state.drawer) return;
    var list = walletList();
    state.drawer.querySelectorAll('[data-dcw-wallet]').forEach(function (btn) {
      var w = list.find(function (x) { return x.id === btn.getAttribute('data-dcw-wallet'); });
      if (!w) return;
      var pill = btn.querySelector('.dcw-pill');
      pill.textContent = w.status;
      pill.className = 'dcw-pill ' + (w.status === 'Detected' ? 'detected' : w.status === 'Recent' ? 'recent' : 'none');
      var spin = btn.querySelector('.dcw-rowspin');
      spin.style.display = state.connecting === w.name ? '' : 'none';
      btn.disabled = !!state.connecting;
    });
  }

  function feedback(msg) {
    var box = state.drawer.querySelector('.dcw-feedback');
    if (!msg) { box.style.display = 'none'; box.textContent = ''; return; }
    var lower = msg.toLowerCase();
    var isErr = lower.indexOf('failed') !== -1 || lower.indexOf('rejected') !== -1 ||
                lower.indexOf('not found') !== -1 || lower.indexOf('not detected') !== -1 ||
                lower.indexOf('incorrect') !== -1 || lower.indexOf('too many') !== -1;
    box.className = 'dcw-feedback ' + (isErr ? 'err' : 'ok');
    box.textContent = msg;
    box.style.display = '';
  }

  function openDrawer() {
    if (!state.drawer) buildDrawer();
    refreshRows();
    feedback('');
    state.overlay.style.display = '';
    state.drawer.style.display = '';
    requestAnimationFrame(function () {
      state.overlay.classList.add('dcw-open');
      state.drawer.classList.remove('dcw-closing');
      state.drawer.classList.add('dcw-open');
    });
    document.body.style.overflow = 'hidden';
  }

  function closeDrawer() {
    if (!state.drawer || state.connecting) return;
    state.overlay.classList.remove('dcw-open');
    state.drawer.classList.remove('dcw-open');
    state.drawer.classList.add('dcw-closing');
    document.body.style.overflow = '';
    setTimeout(function () {
      if (state.drawer) { state.drawer.style.display = 'none'; state.overlay.style.display = 'none'; }
    }, 260);
  }

  function pickWallet(id) {
    var w = walletList().find(function (x) { return x.id === id; });
    if (!w || state.connecting) return;
    state.connecting = w.name;
    feedback('Connecting to ' + w.name + '...');
    refreshRows();
    w.connect().then(function (addr) {
      persist(addr, w.name);
      feedback(w.id === 'phantom'
        ? 'Connected to ' + w.name + '!'
        : 'Connected to ' + w.name + '! Datachain Rope (chainId 271828) added to your wallet.');
      watchProvider(w.name);
      setTimeout(function () {
        state.connecting = null;
        refreshRows();
        closeDrawer();
      }, 1000);
    }).catch(function (err) {
      var msg = (err && err.message) || 'Connection failed';
      if (err && err.code === 4001) msg = 'Connection request rejected.';
      feedback(w.name + ': ' + msg);
      state.connecting = null;
      refreshRows();
    });
  }

  /* ------------------------------------------------------ provider events */
  var watched = false;
  function watchProvider(walletName) {
    if (watched) return;
    var provider = walletName === 'Phantom Wallet' ? getPhantom()
      : walletName === 'XDC Pay' ? getXdcPay()
      : walletName === 'Exodus' ? getExodus()
      : walletName === 'Trust Wallet' ? getTrust()
      : walletName === 'Coinbase Wallet' ? getCoinbase()
      : getMetaMask() || window.ethereum;
    if (!provider || typeof provider.on !== 'function') return;
    watched = true;
    provider.on('accountsChanged', function (accounts) {
      if (accounts && accounts.length) persist(accounts[0], state.walletName);
      else persist('', '');
    });
    provider.on('disconnect', function () { persist('', ''); });
  }

  /* --------------------------------------------------- header buttons */
  function findHeaderButtons() {
    var candidates = document.querySelectorAll(
      '.header-actions .btn-secondary, header .btn-secondary, .connect-btn, [data-dcw-connect]'
    );
    var out = [];
    candidates.forEach(function (b) {
      var txt = (b.textContent || '').trim().toLowerCase();
      if (b.hasAttribute('data-dcw-connect') || txt.indexOf('connect') !== -1 || b.querySelector('.fa-wallet')) {
        out.push(b);
      }
    });
    return out;
  }

  function renderButtons() {
    state.buttons.forEach(function (entry) {
      var btn = entry.btn;
      if (state.address || (state.walletName === 'Datawallet+' && state.email)) {
        var display = state.address ? short(state.address) : state.email.split('@')[0];
        btn.classList.add('dcw-connected-btn');
        btn.innerHTML = SVG_CHECK + '<span class="dcw-addr">' + esc(display) + '</span>';
        btn.title = (state.address || state.email) + (state.walletName ? ' · ' + state.walletName : '');
      } else {
        btn.classList.remove('dcw-connected-btn');
        btn.innerHTML = SVG_WALLET + 'Connect';
        btn.title = 'Connect your wallet';
        closeMenu(entry);
      }
    });
  }

  function buildMenu(entry) {
    if (entry.menu) return entry.menu;
    var wrap = entry.btn.parentElement;
    if (wrap && getComputedStyle(wrap).position === 'static') wrap.style.position = 'relative';
    var menu = document.createElement('div');
    menu.className = 'dcw-menu';
    (wrap || document.body).appendChild(menu);
    entry.menu = menu;
    return menu;
  }

  function closeMenu(entry) {
    if (entry.menu) entry.menu.classList.remove('dcw-open');
  }

  function toggleMenu(entry) {
    var menu = buildMenu(entry);
    if (menu.classList.contains('dcw-open')) { menu.classList.remove('dcw-open'); return; }
    var headLabel = state.address ? short(state.address) : esc(state.email);
    menu.innerHTML =
      '<div class="dcw-menu-head">' + esc(state.walletName || 'Wallet') + ' · <span class="dcw-addr">' + esc(headLabel) + '</span></div>' +
      '<button data-dcw-act="copy">' + SVG_COPY + (state.address ? 'Copy address' : 'Copy email') + '</button>' +
      (state.address ? '<a href="' + EXPLORER_BASE + '/address/' + encodeURIComponent(state.address) + '">' + SVG_EXT + 'View on DCScan</a>' : '') +
      '<button class="dcw-menu-danger" data-dcw-act="disconnect">' + SVG_OUT + 'Disconnect</button>';
    menu.querySelector('[data-dcw-act="copy"]').addEventListener('click', function () {
      var addr = state.address || state.email;
      (navigator.clipboard ? navigator.clipboard.writeText(addr) : Promise.reject()).then(function () {
        this.innerHTML = SVG_CHECK + 'Copied!';
      }.bind(this)).catch(function () {
        var ta = document.createElement('textarea');
        ta.value = addr; document.body.appendChild(ta); ta.select();
        document.execCommand('copy'); ta.remove();
        this.innerHTML = SVG_CHECK + 'Copied!';
      }.bind(this));
      setTimeout(function () { closeMenu(entry); }, 900);
    });
    menu.querySelector('[data-dcw-act="disconnect"]').addEventListener('click', function () {
      var phantom = getPhantom();
      if (state.walletName === 'Phantom Wallet' && phantom && phantom.disconnect) {
        phantom.disconnect().catch(function () {});
      }
      persist('', '');
      closeMenu(entry);
    });
    menu.classList.add('dcw-open');
  }

  function bindButtons() {
    findHeaderButtons().forEach(function (orig) {
      // Clone-replace to strip any legacy inline listeners bound by old page JS.
      var btn = orig.cloneNode(false);
      btn.className = orig.className;
      orig.parentNode.replaceChild(btn, orig);
      var entry = { btn: btn, menu: null };
      state.buttons.push(entry);
      btn.addEventListener('click', function (e) {
        e.preventDefault();
        e.stopPropagation();
        if (state.address) toggleMenu(entry);
        else openDrawer();
      });
    });
    document.addEventListener('click', function (e) {
      state.buttons.forEach(function (entry) {
        if (entry.menu && entry.menu.classList.contains('dcw-open') &&
            !entry.menu.contains(e.target) && e.target !== entry.btn && !entry.btn.contains(e.target)) {
          closeMenu(entry);
        }
      });
    });
    renderButtons();
  }

  /* ------------------------------------------------------ resume session */
  function resumeSession() {
    if (state.walletName === 'Datawallet+') {
      var token = localStorage.getItem(LS_TOKEN);
      if (token && !tokenExpired(token)) {
        renderButtons();
      } else {
        persist('', '');
      }
      return;
    }
    if (!state.address || !state.walletName) return;
    if (state.walletName === 'Phantom Wallet') {
      var ph = getPhantom();
      if (ph) {
        ph.connect({ onlyIfTrusted: true }).then(function (resp) {
          persist(resp.publicKey.toString(), 'Phantom Wallet');
          watchProvider('Phantom Wallet');
        }).catch(function () { persist('', ''); });
      } else { persist('', ''); }
      return;
    }
    var provider = state.walletName === 'XDC Pay' ? getXdcPay()
      : state.walletName === 'Exodus' ? getExodus()
      : state.walletName === 'Trust Wallet' ? getTrust()
      : state.walletName === 'Coinbase Wallet' ? getCoinbase()
      : getMetaMask() || window.ethereum;
    if (!provider) { persist('', ''); return; }
    provider.request({ method: 'eth_accounts' }).then(function (accounts) {
      var live = (accounts || []).map(function (a) { return a.toLowerCase(); });
      if (live.indexOf(state.address.toLowerCase()) !== -1) {
        watchProvider(state.walletName);
        renderButtons();
      } else if (live.length) {
        persist(accounts[0], state.walletName);
        watchProvider(state.walletName);
      } else {
        persist('', '');
      }
    }).catch(function () { persist('', ''); });
  }

  /* ------------------------------------------------------------ init */
  function init() {
    var style = document.createElement('style');
    style.id = 'dcscan-wallet-css';
    style.textContent = CSS;
    document.head.appendChild(style);
    bindButtons();
    resumeSession();
  }

  /* Public API — lets host pages (e.g. the datachain.network landing page's
   * "Add to Datawallet+" button) open the Connect Wallet drawer and push the
   * Rope network config to injected wallets programmatically. */
  window.DatachainWallet = {
    open: openDrawer,
    close: closeDrawer,
    addNetwork: syncRopeNetworkToWallets,
    chain: ROPE_CHAIN,
    isConnected: function () {
      return !!(state.address || (state.walletName === 'Datawallet+' && state.email));
    },
    /* Current connection snapshot for host pages. */
    getState: function () {
      return { address: state.address, walletName: state.walletName, email: state.email };
    },
    /* Datachain ID (Datawallet+) bearer token, when signed in via
     * credentials — lets host backends verify the session server-side
     * against id.datachain.network. */
    getIdentityToken: function () {
      var t = localStorage.getItem(LS_TOKEN);
      return (t && !tokenExpired(t)) ? t : null;
    },
    /* EIP-1193 provider matching the connected wallet (null for the
     * Solana-only Phantom connection or when nothing is connected). */
    getEvmProvider: function () {
      switch (state.walletName) {
        case 'MetaMask':        return getMetaMask() || window.ethereum || null;
        case 'XDC Pay':         return getXdcPay() || null;
        case 'Coinbase Wallet': return getCoinbase() || null;
        case 'Trust Wallet':    return getTrust() || null;
        case 'Exodus':          return getExodus() || null;
        case 'Phantom Wallet':  return null;
        default:                return state.address ? (window.ethereum || null) : null;
      }
    }
  };

  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
})();
