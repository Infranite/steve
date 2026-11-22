Needs a minor release: Clients that read main conversation history must switch from /api/thread/history to /api/views/page?kind=message. Main conversation changes now arrive as message patches.

The main conversation loads its latest messages first and requests older pages as you scroll or jump to a search result. Only the visible part of the conversation renders, and streaming replies update in place. Channel conversations show one reply draft with controls to send, edit, or skip it, while keeping previous versions.

Item lists load compact summaries. Full goals, plans, and progress are fetched on opening the item.
