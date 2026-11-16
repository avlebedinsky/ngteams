# The Python bot side of the bots QA test: msal-python and the Bot Framework SDK's own token checks, pinned.
FROM docker.io/library/python:3.12-slim
RUN pip install --no-cache-dir --root-user-action=ignore \
    msal==1.31.1 botframework-connector==4.16.2 botbuilder-core==4.16.2 aiohttp==3.10.11
