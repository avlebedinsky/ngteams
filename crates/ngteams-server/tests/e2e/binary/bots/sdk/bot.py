"""The Python bot side of the bots QA test, against an ngteams server: msal-python's client credentials, the Bot
Framework SDK's own credentials (parameterized, as a bot on a private cloud configures it) and its own checks of the
token an activity carries. Prints one JSON line; never a token or a secret. All ids are synthetic."""

import asyncio
import base64
import json
import os
import sys


def claims(token):
    part = token.split(".")[1]
    return json.loads(base64.urlsafe_b64decode(part + "=" * (-len(part) % 4)))


def grant(result):
    if "access_token" in result:
        return {
            "ok": True,
            "token_type": result.get("token_type"),
            "expires_in": result.get("expires_in"),
            "claims": claims(result["access_token"]),
        }
    return {"ok": False, "error": result.get("error"), "codes": result.get("error_codes")}


def msal_token():
    import msal

    app = msal.ConfidentialClientApplication(
        os.environ["BOT_ID"],
        client_credential=os.environ["BOT_SECRET"],
        authority=os.environ["AUTHORITY"],
    )
    return grant(app.acquire_token_for_client(scopes=[os.environ["SCOPE"] + "/.default"]))


def authentication(bot_id, secret):
    from botframework.connector.auth import (
        AuthenticationConfiguration,
        BotFrameworkAuthenticationFactory,
        PasswordServiceClientCredentialFactory,
    )

    origin = os.environ["ORIGIN"]
    metadata = origin + "/bot/v1/.well-known/openidconfiguration"
    return BotFrameworkAuthenticationFactory.create(
        channel_service="ngteams",
        validate_authority=False,
        to_channel_from_bot_login_url=os.environ["AUTHORITY"],
        to_channel_from_bot_oauth_scope=os.environ["SCOPE"],
        to_bot_from_channel_token_issuer=os.environ["ISSUER"],
        oauth_url=origin + "/bot",
        to_bot_from_channel_open_id_metadata_url=metadata,
        to_bot_from_emulator_open_id_metadata_url=metadata,
        caller_id="urn:botframework:ngteams",
        credential_factory=PasswordServiceClientCredentialFactory(bot_id, secret),
        auth_configuration=AuthenticationConfiguration(),
    )


def sdk_token():
    from botframework.connector.auth import PasswordServiceClientCredentialFactory

    bot_id = os.environ["BOT_ID"]
    factory = PasswordServiceClientCredentialFactory(bot_id, os.environ["BOT_SECRET"])
    credentials = asyncio.run(
        factory.create_credentials(bot_id, os.environ["SCOPE"], os.environ["AUTHORITY"], False)
    )
    try:
        token = credentials.get_access_token()
    except PermissionError as error:
        return {"ok": False, "error": str(error).split(",")[0]}
    return {"ok": True, "claims": claims(token)}


async def check(case):
    from botbuilder.schema import Activity, ChannelAccount, ConversationAccount

    auth = authentication(case["bot"], "unused-synthetic")
    activity = Activity(
        type="message",
        channel_id="msteams",
        service_url=case["service_url"],
        recipient=ChannelAccount(id="28:" + case["bot"]),
        conversation=ConversationAccount(id="a:synthetic"),
    )
    try:
        result = await auth.authenticate_request(activity, "Bearer " + case["token"])
    except Exception as error:  # the SDK raises several types; the test reads only that it refused
        return {"name": case["name"], "ok": False, "error": type(error).__name__}
    return {
        "name": case["name"],
        "ok": result.claims_identity.is_authenticated,
        "audience": result.audience,
        "caller_id": result.caller_id,
    }


def verify():
    cases = json.load(sys.stdin)

    async def run():
        return [await check(case) for case in cases]

    return asyncio.run(run())


if __name__ == "__main__":
    action = {"msal": msal_token, "sdk-token": sdk_token, "verify": verify}[sys.argv[1]]
    print(json.dumps(action()))
