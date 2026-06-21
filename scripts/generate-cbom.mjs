#!/usr/bin/env node
import { randomUUID } from "node:crypto";
import { writeFileSync } from "node:fs";

const version = process.env.TSFRV_VERSION ?? "0.1.0";
const commit = process.env.GITHUB_SHA ?? "local";

const cbom = {
  bomFormat: "CycloneDX",
  specVersion: "1.6",
  serialNumber: `urn:uuid:${randomUUID()}`,
  version: 1,
  metadata: {
    timestamp: new Date().toISOString(),
    tools: [{ vendor: "masanork", name: "tsfrv-generate-cbom", version: "1.0.0" }],
    component: {
      type: "application",
      name: "tsfrv",
      version,
      description: "Telnet over SSL/TLS 電子公告ビューア (Cloudflare Workers)",
      externalReferences: [
        {
          type: "website",
          url: "https://tsfrv.masanork.workers.dev/",
        },
        {
          type: "vcs",
          url: "https://github.com/masanork/tsfrv",
          comment: commit,
        },
      ],
    },
  },
  components: [
    {
      type: "cryptographic-asset",
      "bom-ref": "crypto:cloudflare-workers-tls",
      name: "Cloudflare Workers outbound TLS",
      description:
        "telnets 接続に Cloudflare connect() API の secureTransport=on を使用",
      cryptoProperties: {
        assetType: "protocol",
        protocolProperties: {
          type: "tls",
          cipherSuites: ["platform-negotiated"],
          cryptoRefArray: ["crypto:cloudflare-platform-trust-store"],
        },
      },
    },
    {
      type: "cryptographic-asset",
      "bom-ref": "crypto:cloudflare-platform-trust-store",
      name: "Cloudflare platform trust store",
      description: "Workers ランタイムが管理するルート証明書ストア",
      cryptoProperties: {
        assetType: "related-crypto-material",
        relatedCryptoMaterialProperties: {
          type: "certificate",
        },
      },
    },
    {
      type: "cryptographic-asset",
      "bom-ref": "crypto:wasm-sigstore-attestation",
      name: "GitHub Artifact Attestation (Sigstore)",
      description: "CI で生成する SLSA Build L3 証明と SBOM 署名",
      cryptoProperties: {
        assetType: "related-crypto-material",
        relatedCryptoMaterialProperties: {
          type: "key",
          algorithm: "ecdsa-with-SHA256",
        },
        oid: "1.3.6.1.4.1.11129.2.4.2",
      },
    },
  ],
};

writeFileSync("sbom/cbom.json", `${JSON.stringify(cbom, null, 2)}\n`);