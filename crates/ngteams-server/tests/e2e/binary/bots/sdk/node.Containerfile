# The Node bot side of the bots QA test: msal-node's confidential client, pinned.
FROM docker.io/library/node:22-slim
WORKDIR /bot
RUN npm install --no-audit --no-fund @azure/msal-node@3.8.1
