# User guide

## Install and update

Get the package for your platform from [Releases](https://github.com/PordoTech/pordo/releases). Windows offers an installer and portable ZIP. macOS 15+ offers separate Apple Silicon and Intel DMG/ZIP packages; move `Pordo.app` to Applications. When upgrading from `replayer.app`, quit the old app and remove its app bundle after installing Pordo; existing settings are kept.

Windows updates can be checked, downloaded and installed from **Settings → About**. On macOS, use the Releases link. Export generated subtitles before restarting.

Service credentials are saved through Windows Credential Manager or macOS Keychain; Android protects its local credentials with Android KeyStore. Local playback and comic reading do not require an AI account.

The repository moved to `PordoTech/pordo`. Windows clients through v0.5.1 still validate installer URLs against the previous repository. Install the first newer version manually from Releases once it is available; later clients use the new location.

## Music and video

Open one or more files or drop them onto the window. **Add Files** or Shift-drop appends files; a normal open/drop replaces the current queue. Music and video queues are saved independently. Removing an item from a queue does not delete its file.

The app chooses music or video mode from the media tracks. Music shows available title, artist, album, cover art and lyrics. Embedded tags take priority; downloaded-track metadata fills gaps. A matching `.lrc` file can supply timed lyrics.

Click the **Lyrics** icon in the playback bar, then choose **Generate AI lyrics** to transcribe a local song or a fully downloaded online track. **Show lyrics** opens the lyric view. Generation uses the audio-capable model selected under **Settings → Video → AI subtitles**, keeps the song's original language, and displays completed lines while generating. On successful completion, lyrics are automatically saved as a matching `.lrc` file beside the song and loaded next time. **Regenerate lyrics** replaces that file after the new generation succeeds. Cancelling, failed segments, or no recognized lyrics leave the saved file unchanged. Existing lyrics remain visible until new lines are recognized. Instrumental passages can produce no lyrics; recognition and timing depend on the selected model. Generating lyrics sends the song's audio chunks to the configured AI service.

Use the queue sidebar to select, remove or reorder entries. Hold an item's title before dragging to reorder. Favorites and recent activity are available from the media pages and Home. **Play all** on music favorites uses the displayed favorites, including online tracks not downloaded on this device.

In **Music → Playlists**, create, rename or delete a playlist. Use a favorite song’s **More → Add to playlist**, or **Add songs** in a playlist to select multiple favorites. Hold a song’s title to reorder it, or use **Move up / Move down**. **Play all** starts the playlist; double-clicking a song starts the full playlist at that song. Removing a song or deleting a playlist keeps favorites and files. Playlist edits do not change an already playing queue. Playlists are stored locally and are not included in cloud sync.

| Shortcut | Action |
| --- | --- |
| Space | Pause/resume media |
| Left / Right | Seek during media playback; turn pages in the comic reader |
| M | Mute/unmute media |
| F11 | Toggle full screen |
| Esc | Leave full screen before other back actions |
| F / double-click | Toggle full screen during video playback |

Video and comic toolbars appear when the pointer approaches the edges. Music controls stay visible.

Music, video and comic playback pages have a settings button immediately before the
window controls. Use **Back** in settings, or click the current media cover, to return
without reopening the media. Unsaved settings remain available until saved or discarded.

Video playback offers **1.0×**, **1.25×**, **1.5×**, **2.0×** and **3.0×** in the lower-right
speed menu. Audio pitch is preserved; the remembered video speed does not change music playback.
While a video is playing (including seeking or buffering), Pordo requests that the operating system keep the display awake. Pausing, ending, stopping or closing playback releases that request.

Music, video and comic searches show progress in the search button without adding a loading row below the input. Comic update checks keep the refresh icon's size while it spins.

On Windows, playback follows the default output when headphones or other audio devices are connected or removed. Short device outages retry automatically. Music waits at its current position; video may continue while audio is unavailable. Longer outages keep retrying, so reconnecting an output does not require reopening the media. Paused video defers audio recovery until playback resumes. Audio recovery notices appear only on the current playback page.

### Online music

Configure the music-source API key in **Settings → Music**. The ChKSz adapter exposes NetEase Cloud Music, QQ Music and Kugou search and playback; availability and quality depend on the upstream service.

**Play** starts playback while downloading when the source permits it. **Download** saves a complete file. Existing downloads are reused; unavailable quality is reported rather than silently replaced. The download directory is configurable, and completed files remain there after playback.

Keep `.pordo.json`, `.pordo-cover.png` (or their older `.replayer` equivalents) and any `.lrc` sidecars with their audio file when moving downloaded music. They contain metadata, cover art and lyrics that may not be embedded in the audio.

### QQ Music local files

Some QMC/MFLAC/MGG variants are supported. If the player requests a song key, use **Import song key and retry**, or put its Base64 ekey beside the media as `song.mflac.ekey`, retaining the original extension. This is a per-song key; the player does not retrieve account credentials or keys from other applications.

Legacy QMC1 and supported QMC2 Map/RC4 variants can be opened. Some metadata-only variants require an external ekey. EncV2 and unknown footer versions remain unsupported; the extension alone does not establish compatibility. Finish downloading encrypted torrent files before opening them locally.

### Video updates

In **Video → Updates**, new uploads from favorite Bilibili creators are grouped by creator. The first successful check establishes a baseline without marking older uploads as new. Use the refresh icon to check manually; automatic checks run at startup or when entering Home or Video when due, with a 15-minute interval and a one-hour delay after failures. Disable automatic checks in Video settings if preferred.

Groups show up to six unseen uploads initially and can expand to show the rest. Open a card to play, use its menu to mark it as seen, or mark the whole group as seen and undo within eight seconds. Successful playback marks a video as seen after playback progresses; opening the page or a failed playback attempt does not. Progress remains separate from these update reminders. Large backlogs offer **Continue checking** after each five-page batch.

Update lists, baselines and seen markers use the existing cloud sync and merge between devices. No separate local update library is created. Without cloud sync enabled, these states last only for the current session. A new device waits for its first successful cloud sync before establishing missing baselines. Request queues, failure delays and pagination cursors remain session state; after restart, checks rescan from the first page and deduplicate against the synced content.

### Magnet links

Choose **Open magnet link**, wait for metadata and select a supported file. Playback waits for verified downloaded data; seeking prioritizes the requested location. Poor peer availability or a proxy blocking BT traffic can delay playback.

The task menu supports file selection, pause/resume and stop. Pausing the download also pauses playback. Stopping retains downloaded files; torrent sessions do not continue after the app exits. Cache files are not automatically removed. Pure v2 magnets without a v1 `btih` hash are not supported.

AI subtitles can use audio already present in the torrent cache. Missing audio may delay generation. For TUN/proxy setups, use the routing guidance offered by the magnet dialog; keep any AI-service proxy rule ahead of a process-wide direct rule.

## Comics

Search MangaYun or Komiic, open a title and choose a chapter. Sources remain separate even when titles match. Add books to your bookshelf, check updates and resume from history. Komiic chapters can be filtered by chapter/volume type.

Favorite updates are checked at startup and when returning home, at least 15 minutes apart. Komiic checks up to 50 update summaries in one request, shows the latest chapter/volume and keeps an update dot without downloading directories or images. Opening a title's details fetches its directory and resolves exact unread chapter counts. MangaYun uses directory checks with at most four background requests overall, two per upstream source and 500 ms between request starts for each source. Each background request has a 20-second limit; errors leave cached data available. Automatic checks revisit completed titles after a day and failed titles after an hour. Manual refresh bypasses those intervals.

Choose vertical scrolling, left-to-right or right-to-left pages. Zoom, page position and reading progress are saved. The chapter button opens the chapter list; the page slider and previous/next controls navigate within the book. A new favorite starts with the existing chapter list as its baseline; later unread additions produce update indicators. Each favorite comic remembers its own ascending or descending detail-directory order on this device. The default is ascending; this display preference does not change previous/next chapter navigation.

Finishing the latest chapter or volume dismisses that comic's current update notice, even if earlier chapters remain unread. Earlier chapters keep their actual read/unread status. New chapters make the comic appear in Updates again. The dismissal is saved and follows reading-progress cloud sync.

Original pages and covers are cached. Clearing these caches in Settings preserves translations, favorites and history. Previously cached pages may remain readable when a source is unavailable or its image quota is exhausted. Some upstream sources require adapter updates or are unsupported.

### Translate comic pages

Add a compatible service in **Settings → AI Providers**, then select an image-editing model in the comic settings. The service must accept an image-edit request and return an edited image; a text-only model is insufficient.

Choose **Translate page** or **Translate chapter**. Original, translated and side-by-side views are available. Original images are preserved, and cached translations can be reused. Old PNG/WebP translations remain usable; new translated results use JPEG. A failed page does not remove its original or prevent later pages from continuing.

Translation is started explicitly. Cancelling stops local waiting, but an already accepted request may still be billed. Generated images can change artwork or mistranslate text; compare with the original.

## Bilibili videos (desktop)

The danmaku button to the right of Subtitles opens a panel with a visibility switch, quarter/half/full
picture coverage, opacity, font size and scrolling speed. Changes apply immediately
and are remembered. Ordinary scrolling and top/bottom comments follow playback,
pauses, speed changes and seeking. Comments stay within the picture and leave room
for subtitles and controls. Animated/scripted comments and posting are not supported.
Some videos return fewer comments without signing in; a danmaku error does not stop playback.

Open **Videos → Search**, using the same Favorites / Search tabs as the music page. Choose the source, then select **All**, **Videos** or **Creators** to the left of the search box. Search by keyword, or paste a BV ID or a full `bilibili.com/video/…` link. Creator results open their uploaded videos in the same page. Video covers show play counts at the lower left and duration at the lower right. Search cards show the UP name and publication time below the title; a creator's upload list omits the repeated UP name. Recent publication times use relative labels, older dates use month/day within the current year and a full date in earlier years. Selecting a video immediately opens the player, where metadata and streams load, preferring 1080P, then 720P or a lower authorized AVC stream when needed. The lower-right controls are Quality, Speed, Subtitles, Danmaku, Volume, Play queue and Fullscreen; local videos hide Quality and Danmaku. The play-queue sidebar lists the current video's parts as cover cards with names and durations, highlights the playing part and switches parts on click. Changing quality preserves playback position and pause state, while a different part starts from its beginning. Recent videos can be reopened from Home and the video library.

The heart on a creator card or uploaded-video page saves that creator to **Videos → Favorites**, separately from playable videos. Favorites survive restarts and participate in configured cloud sync; account cookies and login sessions remain device-local. Search preserves Bilibili's returned order without adding a local popularity or publication-date sort; mixed results show the creator section before videos.

In **Settings → Video**, select **Bilibili**, choose **Sign in with QR**, scan with the official Bilibili app and confirm on your phone. The account name appears after confirmation. Sessions are saved in Windows Credential Manager or macOS Keychain, outside ordinary settings and cloud sync. **Sign out** appears while signed in and removes the locally saved session and stops Bilibili playback. Expired sessions require another scan; account-password login is not offered. Login controls are kept in settings; the video search bar uses the same layout as music search.

The first version supports ordinary uploaded videos with H.264 video and AAC audio. Available qualities depend on your account and the streams Bilibili returns. Bangumi, live streams, paid courses, HDR and AV1/HEVC-only streams are not supported. If Bilibili requests security verification, follow the website prompt or sign in and retry; the app does not treat that response as an empty search. Initial seeks in long fragmented videos can take longer while the media index is read.

Under **Settings → Video → Bilibili**, **Auto-skip intro** and **Auto-skip outro** independently enable OP/ED skipping; both default to off. Pordo looks up [BilibiliSponsorBlock community markers](https://github.com/hanydd/BilibiliSponsorBlock/wiki/API) using a short hash prefix, without sending Bilibili login cookies. Coverage depends on community submissions. Missing markers or lookup failures do not interrupt playback.

Available OP ranges are gold and ED ranges purple on the playback timeline, even with automatic skipping disabled. Vertical ticks mark each range's start and end. Markers are matched to the current video's BV ID, part CID and duration. Seeking back into a marked range lets you watch it without another automatic skip during that playback pass. Changing quality preserves the markers; switching parts loads that part's markers.

## AI subtitles

In **Settings → AI Providers**, enter a compatible service address, key and available models. Fetch the model list or add an ID manually. Select an audio-capable model under **Settings → Video → AI subtitles**; comic translation selects its model separately.

Open a video and choose **Subtitles → Generate AI subtitles**. Completed segments appear during playback. The subtitle menu can hide, cancel or export the results. Export needed results as SRT before closing or switching media: generated subtitles are not automatically saved.

The interface language selects Chinese or English output. Optional bilingual subtitles show the original and target language. Generation prioritizes the current playback position. MP3 audio is the default, with WAV available for compatible services; model support varies between providers.

Only explicitly requested audio segments are sent to the selected AI service, not video frames or the local file path. Timing and wording are model estimates, and failed segments may leave gaps. Check the result before relying on it. Cancelling a request does not guarantee cancellation of server-side charges.

## Cloud sync and local data

Configure an HTTPS WebDAV service in **Settings → Cloud Sync**, test the connection, then enable sync. Use the same account on another device. Offline changes stay local and are merged after reconnecting.

Sync runs on startup, manual requests and changes to favorites or settings, without periodic polling. Reading and playback progress is saved locally and submitted when leaving the reader/player or closing the app. Unchanged device files are skipped using their ETags. Desktop shutdown waits up to eight seconds for pending sync; unfinished uploads remain saved for the next startup.

Sync covers supported service/model settings, comic and online-music favorites, favorite Bilibili creators, creator update lists and seen markers, reading/listening progress and related preferences. It does not upload local media files or paths, playback queues, downloaded audio, covers, comic pages, translated images or subtitle caches.

API keys are excluded by default. To sync them, enable the option and use the same separate sync passphrase on all devices; keep the original passphrase available when changing it. WebDAV passwords and the passphrase itself are not uploaded. A wrong passphrase stops writes rather than replacing existing encrypted keys.

Windows data normally lives in `%LOCALAPPDATA%/pordo`. macOS defaults to `~/.config/pordo`; a configured `XDG_CONFIG_HOME` changes that location. Existing profiles in the corresponding `replayer` directory are reused automatically. Downloads and caches can occupy significant space. Stop a torrent before manually deleting its cache, and retain downloaded-music sidecars when moving files.

The app does not read development `.env` files. Provider keys belong in the app's supported credential settings. See [third-party notices](third-party.md) for bundled components and [Releases](https://github.com/PordoTech/pordo/releases) for changes.
