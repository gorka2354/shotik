'use strict';
// Signs a release: writes latest.json and latest.json.sig next to the installer. Only Node's own modules, so what
// handles the private key can be read in one sitting.
//
//   node installer/sign-update.js <installer> <version> <outDir>     SHOTIK_UPDATE_KEY holds the key
//   node installer/sign-update.js --keygen <envFile>                 adds a new key to the file, prints the public one
//
// The format is documented in docs/rust/UPDATES.md and read by crates/shotik-core/src/update.rs.
const crypto = require('crypto');
const fs = require('fs');
const path = require('path');

const DOMAIN = 'shotik-update-manifest-v1\n';
// The fixed DER header of a PKCS#8 Ed25519 key, followed by its 32 byte seed.
const PKCS8_ED25519 = Buffer.from('302e020100300506032b657004220420', 'hex');
const SPKI_ED25519 = Buffer.from('302a300506032b6570032100', 'hex');

function privateKeyFrom(seedHex) {
  if (!/^[0-9a-f]{64}$/.test(seedHex || '')) throw new Error('the signing key must be 64 lowercase hex characters');
  return crypto.createPrivateKey({ key: Buffer.concat([PKCS8_ED25519, Buffer.from(seedHex, 'hex')]), format: 'der', type: 'pkcs8' });
}

function publicKeyHex(seedHex) {
  const der = crypto.createPublicKey(privateKeyFrom(seedHex)).export({ format: 'der', type: 'spki' });
  return der.subarray(-32).toString('hex');
}

/** What the application receives: the manifest's exact bytes, and the signature over them. */
function signRelease(installer, version, seedHex) {
  if (!/^\d+\.\d+\.\d+$/.test(version)) throw new Error('the version must look like 2.1.0: ' + version);
  const manifest = Buffer.from(JSON.stringify({
    version,
    sha256: crypto.createHash('sha256').update(installer).digest('hex'),
    size: installer.length,
  }));
  const signature = crypto.sign(null, Buffer.concat([Buffer.from(DOMAIN), manifest]), privateKeyFrom(seedHex));
  return { manifest, signature: Buffer.from(signature.toString('hex') + '\n') };
}

function embeddedPublicKey() {
  const source = fs.readFileSync(path.join(__dirname, '..', 'crates', 'shotik-core', 'src', 'update.rs'), 'utf8');
  const found = source.match(/UPDATE_PUBLIC_KEY: &str = "([0-9a-f]{64})"/);
  if (!found) throw new Error('UPDATE_PUBLIC_KEY not found in update.rs');
  return found[1];
}

/**
 * Writes latest.json and latest.json.sig. Copies in the field would reject a signature from any key but theirs: better
 * to fail here than to find out from them.
 */
function signFiles(installerPath, version, outDir, seed) {
  const expected = embeddedPublicKey();
  if (publicKeyHex(seed) !== expected) throw new Error('the signing key does not match UPDATE_PUBLIC_KEY in update.rs');
  const { manifest, signature } = signRelease(fs.readFileSync(installerPath), version, seed);
  const publicKey = crypto.createPublicKey({
    key: Buffer.concat([SPKI_ED25519, Buffer.from(expected, 'hex')]), format: 'der', type: 'spki',
  });
  const signed = Buffer.concat([Buffer.from(DOMAIN), manifest]);
  if (!crypto.verify(null, signed, publicKey, Buffer.from(signature.toString().trim(), 'hex'))) {
    throw new Error('the signature does not verify');
  }
  fs.writeFileSync(path.join(outDir, 'latest.json'), manifest);
  fs.writeFileSync(path.join(outDir, 'latest.json.sig'), signature);
  return { manifest, signature };
}

function main(args) {
  if (args[0] === '--keygen') {
    const envFile = args[1];
    if (!envFile) throw new Error('usage: --keygen <envFile>');
    if (fs.existsSync(envFile) && /SHOTIK_UPDATE_KEY=/.test(fs.readFileSync(envFile, 'utf8'))) {
      throw new Error(envFile + ' already has a key: replacing it would cut off every installed copy');
    }
    const seed = crypto.randomBytes(32).toString('hex');
    fs.appendFileSync(envFile, `SHOTIK_UPDATE_KEY=${seed}\n`);
    console.log(publicKeyHex(seed));
    return;
  }
  const [installerPath, version, outDir] = args;
  if (!outDir) throw new Error('usage: <installer> <version> <outDir>');
  const seed = (process.env.SHOTIK_UPDATE_KEY || '').trim();
  if (!seed) throw new Error('SHOTIK_UPDATE_KEY is not set');
  const { manifest } = signFiles(installerPath, version, outDir, seed);
  console.log('signed ' + manifest.toString());
}

if (require.main === module) {
  try { main(process.argv.slice(2)); } catch (e) { console.error(e.message); process.exit(1); }
}
module.exports = { signRelease, signFiles, publicKeyHex, DOMAIN };
