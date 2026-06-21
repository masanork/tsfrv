tsfrv: Telnet over SSL/TLS 電子公告ビューア
===

[![CI](https://github.com/masanork/tsfrv/actions/workflows/ci.yml/badge.svg)](https://github.com/masanork/tsfrv/actions/workflows/ci.yml)
[![Supply Chain](https://github.com/masanork/tsfrv/actions/workflows/supply-chain.yml/badge.svg)](https://github.com/masanork/tsfrv/actions/workflows/supply-chain.yml)
[![Live Demo](https://img.shields.io/badge/demo-tsfrv.masanork.workers.dev-blue)](https://tsfrv.masanork.workers.dev/)
[![SLSA 3](https://slsa.dev/images/gh-badge-level3.svg)](https://slsa.dev)

## ライブ版

**https://tsfrv.masanork.workers.dev/**

Cloudflare Workers 上で動作する Rust WASM 版です。

| エンドポイント | 説明 |
|---|---|
| [`/`](https://tsfrv.masanork.workers.dev/) | ヘルプ |
| [`/stream`](https://tsfrv.masanork.workers.dev/stream) | 公告をテキストストリームで表示（デフォルト: `koukoku.shadan.open.ad.jp:992`） |
| `/stream?server=HOST&port=PORT` | 任意の telnets サーバーへ接続 |
| `/ws?server=HOST&port=PORT` | WebSocket で双方向セッション |

## 概要

Telnet over SSL/TLS プロトコルで提供される電子公告を閲覧するツールです。デフォルトでは[「一般社団法人サイバー技術・インターネット自由研究会」の電子公告システム](telnet://koukoku.shadan.open.ad.jp)に接続します。

## ローカル開発

```bash
npm ci
npm run dev      # wrangler dev
npm run deploy   # Cloudflare へデプロイ
```

## サプライチェーン

`main` ブランチへの push ごとに GitHub Actions で以下を生成・署名します。

| 成果物 | 内容 |
|---|---|
| SLSA Build L3 | `actions/attest` によるビルド provenance（Sigstore 署名） |
| SBOM (Rust) | CycloneDX — `cargo cyclonedx` |
| SBOM (npm) | CycloneDX — `@cyclonedx/cyclonedx-npm` |
| CBOM | CycloneDX 1.6 — TLS 等の暗号資産一覧 |

Actions の **Supply Chain** ワークフローから `tsfrv-supply-chain` アーティファクトをダウンロードできます。

### 検証例

```bash
gh attestation verify dist/tsfrv-worker.tar.gz --owner masanork --repo tsfrv
```

## 残課題

- チャット記録の整形

## ライセンス

MIT License