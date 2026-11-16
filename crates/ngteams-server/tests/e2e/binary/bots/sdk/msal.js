// The Node bot side of the bots QA test: msal-node's confidential client against an ngteams server's tenant
// authority. Prints one JSON line; never a token or a secret. All ids are synthetic.
const msal = require("@azure/msal-node");

function claims(token) {
  return JSON.parse(Buffer.from(token.split(".")[1], "base64url").toString());
}

async function main() {
  const authority = process.env.AUTHORITY;
  const app = new msal.ConfidentialClientApplication({
    auth: {
      clientId: process.env.BOT_ID,
      clientSecret: process.env.BOT_SECRET,
      authority,
      knownAuthorities: [new URL(authority).host],
    },
  });
  try {
    const result = await app.acquireTokenByClientCredential({ scopes: [process.env.SCOPE + "/.default"] });
    console.log(JSON.stringify({ ok: true, token_type: result.tokenType, claims: claims(result.accessToken) }));
  } catch (error) {
    console.log(JSON.stringify({ ok: false, error: error.errorCode, codes: error.errorNo ? [Number(error.errorNo)] : null }));
  }
}

main();
