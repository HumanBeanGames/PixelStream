# PixelStream Chat API

PixelStream chat is local-browser chat for custom-host games. It is not tied to
Twitch or any external service.

## App To Chat

Use `StreamChatSender` from Bevy systems when the game needs to write into the
browser chat.

Common patterns:

- `send(text)` posts a short-lived global system entry.
- `send_local(LocalChatEntryOptions)` gives explicit control over audience,
  mentions, display-name color, message color, CSS class, and expiry.
- `for_viewer_identity(identity)` scopes a message to one browser/device viewer.
- `for_viewer_name(name)` scopes a message to the current viewer name mapping.
- `with_mentions(["@name"])` lets the browser highlight local mentions.
- `with_expires_at_ms(...)` or related TTL helpers make transient feedback
  vanish automatically.

Global messages are useful for world events. Viewer-scoped messages are useful
for command replies, private feedback, delayed information, and local UI hints.

All browser-rendered chat text is emitted through text nodes. Color and CSS
class values are validated before they are used by the page.

## Moderation

Viewer-authored browser chat is checked before it is added to history or routed
as a command. The baseline policy rejects:

- links, including URL-looking domains and email-style addresses;
- a small built-in profanity/slur list;
- messages sent faster than one every 5 seconds;
- the same normalized message repeated within 30 seconds.

Rejected messages are never echoed and never reach `StreamChatCommand` handlers.
The viewer receives a private short-lived system warning explaining why the
message was blocked. Moderation state is in-memory and resets with the app.

## Chat To App

Register commands with:

```rust
app.add_stream_command("market", handle_market_command);
```

Command systems receive `StreamChatCommand`, which includes:

- command name and arguments;
- stable viewer identity;
- display name;
- role flags;
- source message id when available.

Reply through `StreamChatSender` and include the player tag in the message body,
for example `@Aster Your caravan is already travelling.`

## Delayed Messages

PixelStream deliberately does not impose a gameplay delay policy. Downstream
games should schedule delayed delivery themselves, then publish the final
message with `StreamChatSender` when the information becomes available to that
viewer.

Mercantile uses this for courier-delay chat and world-event announcements.
