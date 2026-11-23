# Release plan: Pigeon mobile (Android + iOS)

How to get from today's debug builds to the Google Play Store and the Apple App Store.
**Testing comes first**: internal test builds, then a beta, then a staged public release.
This is the working plan for ROADMAP M6.5. Tick items off here as they land, and move
anything structural into the doc that owns it ([`OPERATIONS.md`](OPERATIONS.md) for
build/CI facts, [`SECURITY_REVIEW.md`](SECURITY_REVIEW.md) for security behaviour).

> **Written 2026-11-23** from a check of the code and the server's client API.
> Store rules change: re-check every number and policy below in Play Console or App
> Store Connect when you reach it.

---

## Part 0: shared blockers (both stores, before any public build)

| # | Blocker | Why | Status (2026-11-23) |
|---|---|---|---|
| **S1** | **A public production homeserver** with HTTPS on a real domain | Reviewers and testers need a server they can reach. Release builds refuse `http://`, as they should. | ❌ Only the local docker-compose server exists. Deploy one following [`../pigeon/docs/OPERATIONS.md`](../../pigeon/docs/OPERATIONS.md). |
| **S2** | **A pre-filled production server URL in release builds** | Android release builds still pre-fill `http://10.0.2.2:8008` (the emulator address, `AuthScreen.kt`), which release builds refuse. iOS pre-fills a bare `https://` on devices (`AuthView.defaultServer`). A reviewer would hit an error immediately. | ❌ Small app change on both platforms: a build-type-specific default. |
| **S3** | **In-app account deletion** | Apple guideline 5.1.1(v): an app that lets users create accounts must let them delete them in the app. Google Play requires the same, plus a web link for deletion requests. | ❌ **The server has no account-deletion endpoint.** This needs server work in `../pigeon`, then core and UI on both platforms. |
| **S4** | **Chat-app safety features:** report content, block users, leave rooms, terms of use | Apple 1.2 and Google's user-generated-content policy require reporting and blocking for apps where users talk to each other. Apple also wants users to accept terms. | ❌ **The server has no block, report or leave endpoints.** Because messages are end-to-end encrypted, a report has to carry content the reporter chooses to share, since the server can't read it. That's a design question. |
| **S5** | **Privacy policy URL** plus data-collection disclosures | Required by both stores. They need precise answers: the server stores usernames, device ids, IPs and encrypted blobs; message content is end-to-end encrypted; there's no analytics or ads. | ❌ The policy needs writing and hosting. |
| **S6** | **Encryption export compliance** | MLS counts as "non-exempt" encryption (it goes beyond HTTPS). App Store Connect asks about it, and US export rules (probably the mass-market category) may need a self-classification report. Distributing in France may need a declaration. | ❌ Get someone qualified to confirm. This is a legal question, not a code one. |
| **S7** | **Final app id** | `com.pigeon.mobile` can't be changed after publishing on either store. It should be a reverse-DNS name you control, such as `com.yourdomain.pigeon`. | ⚠️ Decide before creating the store listings. |
| **S8** | **Support contact:** email and support URL | Required by both stores. | ❌ |

**S3 and S4 are the long poles**: they need server changes in `../pigeon`, then core and
UI work on both platforms. The rest is setup or small app changes.

Not blocking, but put it in the reviewer notes: **there is no push.** Messages only arrive
while the app is open, because the server has no push contract (ROADMAP M4.4). That's
acceptable for a beta, but testers will notice.

---

## Part 1: Android (Google Play)

### A0. Accounts
- [ ] **Google Play developer account:** a one-time $25 fee plus identity verification.
- [ ] **Personal vs organization account.** A **personal** account created after Nov 2023
  must run a **closed test with at least 12 opted-in testers for 14 consecutive days**
  before it can publish to production. An **organization** account (which needs a
  D-U-N-S number) is exempt. Either way, the closed test fits the test-first plan.

### A1. Build readiness
| Done | Item | Detail |
|---|---|---|
| [ ] | **Signing** | Create an upload key (keystore). Add a `signingConfig` that reads its path and passwords from environment variables, never from git. Enroll in **Play App Signing**: Google holds the app signing key, and you keep the upload key. |
| [ ] | **App bundle (AAB)** | New apps must upload a `.aab`, not an APK. Check that `bundleRelease` picks up the release core build (only `assembleRelease` has been checked so far). |
| [ ] | **16 KB page size** | Play requires 16 KB-aligned native libraries for apps targeting Android 15+. ✅ `libpigeon_mobile_core.so` is aligned on both ABIs. ❌ JNA 5.14.0's `libjnidispatch.so` is **4 KB-aligned on x86_64**. Upgrade JNA and re-check the APKs generated from the bundle (see "Checking 16 KB alignment" below). |
| [ ] | **R8 / minify** | `isMinifyEnabled = false` today. Turn it on with keep rules for `uniffi.**` and `com.sun.jna.**` (JNA finds classes and methods by name at runtime), then test on a device. Do this *before* the closed test so testers test what ships. |
| [ ] | **Pre-filled URL** | S2. |
| [ ] | **Version codes** | Every upload needs a higher `versionCode` (today: `1` / `0.1.0`). Derive it from the CI run number or a tag. |
| ✅ | **Target SDK** | 36 meets Play's current minimum. |
| ✅ | **Cleartext HTTP** | Refused in release builds (`de40915`). |
| [ ] | **32-bit `armeabi-v7a`** | Your decision. Skipping it means 32-bit-only phones can't install the app. |
| — | **`security-crypto` library** | It's an alpha version (`1.1.0-alpha06`), and Google has deprecated the library. Not a blocker; plan a replacement after launch. |

**Checking 16 KB alignment.** Every `LOAD` segment of every `.so` in the APK must be
aligned to at least 16384:

```bash
python3 -I - path/to/app.apk <<'EOF'
import zipfile, struct, sys
z = zipfile.ZipFile(sys.argv[1])
for i in z.infolist():
    if not i.filename.endswith('.so'): continue
    d = z.read(i)
    phoff = struct.unpack_from('<Q', d, 0x20)[0]
    phentsize, phnum = struct.unpack_from('<HH', d, 0x36)
    aligns = [struct.unpack_from('<Q', d, phoff + k*phentsize + 0x30)[0]
              for k in range(phnum)
              if struct.unpack_from('<I', d, phoff + k*phentsize)[0] == 1]
    print(f"{i.filename:55s} {min(aligns)}")
EOF
```

### A2. Store listing
- [ ] App name; short description (80 chars); full description
- [ ] 512×512 icon (✅ exists); 1024×500 feature graphic; at least 2 phone screenshots
- [ ] Content-rating questionnaire (a chat app with user content)
- [ ] Target audience: declare 13+ or 18+, not children, to stay out of the Families program
- [ ] **Data safety** form (keep it consistent with Apple's App Privacy labels)
- [ ] Privacy policy URL (S5) and account-deletion URL (S3); "no ads"
- [ ] **App access** instructions: a demo account plus the production server URL, so
  reviewers can sign in

### A3. Testing tracks: the test-first rollout
1. **Internal testing** (up to 100 testers, available within minutes, no review)
   - Your own devices. Play's **pre-launch report** also runs the app automatically on
     real devices and flags crashes and accessibility issues.
   - *Move on when:* no crashes in the pre-launch report, and two people on different
     devices hold an encrypted conversation.
2. **Closed testing** (the 12 testers × 14 days gate; plan for 2–4 weeks). Run the
   [shared beta test plan](#shared-beta-test-plan), plus:
   - The oldest supported Android version (minSdk 24 = Android 7) and a low-end phone
   - Crash data comes from Play Console's **Android vitals**, with no crash-reporting SDK.
     That keeps Gotcha #2 (no plaintext in crash reports) safe.
   - *Move on when:* no crash-rate or ANR alerts in vitals, no open data-loss or crypto
     bugs, and the 14-day requirement is met.
3. **Open testing** (optional): a public beta link if you want wider feedback before
   production.
4. **Production with a staged rollout:** 5% → 20% → 50% → 100% over about a week. Halt the
   rollout if vitals worsen. You can stop a rollout, but you can't take back a version; a
   fix ships as a higher version.

### A4. CI/CD (after the first manual release)
- [ ] Build a signed AAB on a git tag, with the keystore stored as encrypted GitHub secrets.
- [ ] Upload to the internal track through the Play Developer API. That needs a tool such
  as `gradle-play-publisher` or `fastlane supply`, which would be a **new dependency to
  approve**. The first upload has to be manual in Play Console anyway.

---

## Part 2: iOS (App Store)

### I0. Accounts
- [ ] **Apple Developer Program:** $99 per year. An individual account shows your personal
  name as the seller; an organization account needs a D-U-N-S number.
- [ ] Register the final **bundle id** (S7) and create the app record in App Store
  Connect. Accept the free-apps agreement.

### I1. Build readiness
| Done | Item | Detail |
|---|---|---|
| [ ] | **Signing** | Set `DEVELOPMENT_TEAM`, and keep automatic signing for local builds. The Keychain access-group entitlement (`Pigeon/Pigeon.entitlements`) gets the real team-id prefix. CI needs a distribution certificate, a provisioning profile and an App Store Connect API key, stored as secrets. |
| [ ] | **Archive and upload** | Add an `ios/archive.sh`: `build-core.sh` (already builds the core in release mode), then `xcodebuild archive`, then `-exportArchive` with an `ExportOptions.plist`, then upload with an App Store Connect API key. |
| [ ] | **Export compliance** | Add `ITSAppUsesNonExemptEncryption = YES` through an `INFOPLIST_KEY_` build setting (the project generates its Info.plist), plus the documentation from S6. Otherwise every build waits on the compliance question. |
| [ ] | **Privacy manifest** | Add `PrivacyInfo.xcprivacy`. The Rust core probably uses "required-reason" APIs: SQLite reads file timestamps (`stat`/`fstat`), and timers may read the system uptime. The first upload triggers an email (ITMS-91053) listing exactly what's missing, so use that to fill the manifest precisely. |
| [ ] | **iPad** | The app is currently **iPhone + iPad** (`TARGETED_DEVICE_FAMILY = 1,2`). That means iPad screenshots, and reviewers check the iPad layout. **Suggested for v1: iPhone-only** (iPads still run it in compatibility mode); add iPad later. |
| [ ] | **Pre-filled URL** | S2. |
| [ ] | **Build numbers** | `CFBundleVersion` must increase with every upload (today: `1.0` build `1`). |
| ✅ | **SDK** | Xcode 27.2 meets Apple's current SDK requirement. Minimum iOS is 15. |
| [ ] | **Background modes** | If `SyncController` declares any background mode, the review notes must justify it. Check before submitting. |

### I2. App Store Connect metadata
- [ ] Name (30 chars), subtitle, description, keywords
- [ ] Support URL and privacy policy URL
- [ ] Screenshots for the 6.9" iPhone (plus 13" iPad if universal)
- [ ] **Age-rating** questionnaire (chat with user content)
- [ ] **App Privacy** labels (the same facts as Play's Data safety form)
- [ ] **App Review info:** a demo account, the production server URL, a note that messages
  are end-to-end encrypted, and a note that there's no push

### I3. TestFlight: the test-first rollout
1. **Internal testers** (up to 100 people on your App Store Connect team, no review,
   available right after processing). Same exit bar as Android's internal track.
2. **External testers** (up to 10,000). The first build of each version goes through a
   short **Beta App Review**, and you can share a public link. Run the
   [shared beta test plan](#shared-beta-test-plan), plus:
   - **Delete and reinstall.** The Keychain survives deleting the app, so a reinstall may
     find an old session and store key without the database. The code is built to rebuild
     from the server in that case; confirm it on a real device.
   - Lock the phone during a sync (the store's `completeUnlessOpen` file protection); low
     storage; the oldest supported iOS version (15).
   - Builds expire after 90 days, so plan regular builds.
   - Crash data: TestFlight crash reports and Xcode's Organizer, with no SDK.
3. **App Store release:** submit for review. Choose **manual release**, so you press the
   button after approval, and **phased release** (spread over 7 days to people with
   automatic updates). You can pause it, but not roll back.

### I4. CI/CD (after the first manual release)
- [ ] Archive and upload on the macOS runner when a git tag is pushed, using the App Store
  Connect API key. Tools like `fastlane` are optional and would be a **new dependency to
  approve**.

---

## Shared beta test plan

Run it on both platforms during the closed test and the TestFlight external beta:

- [ ] Encrypted conversations between 2 and 3 people, including someone added later (the
  MLS commit path), and across platforms (Android ↔ iOS)
- [ ] Going offline, then online: airplane mode mid-send; resuming after a long time offline
- [ ] Backup, delete the app, reinstall and restore (on a second phone if possible)
- [ ] Sign out and back in
- [ ] Images in plaintext and encrypted rooms
- [ ] **Upgrading from one beta build to the next with existing data** (store migrations
  must not lose history)
- [ ] Battery drain while the app stays open (the sync loop)
- [ ] Account deletion, reporting and blocking (once S3/S4 exist)

The hands-on procedures (wiping app data, backup/restore) are in the server repo's
[`LOCAL_TESTING.md`](../../pigeon/docs/LOCAL_TESTING.md).

---

## Part 3: order of work

| Stage | Android | iOS | Gate to move on |
|---|---|---|---|
| **0. Decide** | App id; personal or organization account | App id; individual or organization; iPhone-only or universal | Decisions made; accounts paid for |
| **1. Blockers** | S1 server deployed, S2, S5, S6, S8. **S3/S4 need server work** | (shared) | A reviewer could sign in and use the app |
| **2. Builds** | A1 | I1 | A signed AAB and archive install on a real device |
| **3. Internal** | Internal track | TestFlight internal | Encrypted conversation across platforms; no crashes |
| **4. Beta** | Closed test, ≥ 14 days | TestFlight external | Test plan passed, no data-loss or crypto bugs, store requirements met |
| **5. Release** | Staged rollout | Phased release | Healthy crash and ANR numbers |

Android's 14-day closed-test requirement sets the pace. Start it as soon as Stage 2 is
done, and run the iOS external beta in the same window.

## Who does what

- **Owner (non-code):** the developer accounts, D-U-N-S numbers if going the organization
  route, the domain and server deployment, the privacy policy and terms (legal text),
  export-compliance sign-off, recruiting testers, the signing keys, and approving S3/S4
  server changes in `../pigeon`.
- **Code:** everything in A1 and I1; the pre-filled release URL; S3/S4 in the server, core
  and both apps; draft store text and disclosure answers for the owner to review; the
  release scripts and CI pipeline.
