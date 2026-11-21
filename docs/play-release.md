# Releasing Hopwyn on Google Play

What is ready in the repository, and the steps that need the Play Console.

## Ready in the repository

- **Target SDK 36**, which Play requires for new apps and updates.
- **16 KB page-aligned native library** (NDK r28), which Play requires for apps that target Android 15 or later.
- **R8 shrinking** for release builds. `android/app/proguard-rules.pro` keeps the JNI class the Rust core binds to.
- **App Bundle**: `scripts/build-android.sh bundle` writes `android/app/build/outputs/bundle/release/app-release.aab`.
- **No restricted permissions.** Battery optimisation is changed from Hopwyn's App info page, not with `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS`, which Play allows only for a few kinds of app.
- **Prominent disclosure** before Accessibility settings open: the user must tap **Agree and turn on**, as Play's Accessibility policy requires.
- **Privacy policy**: [docs/privacy-policy.md](privacy-policy.md). Its public URL is
  `https://github.com/Dhiva-Labs/hopwyn/blob/main/docs/privacy-policy.md`.
- **Store listing text**: `android/fastlane/metadata/android/en-US/`.

## 1. Create the upload key (once)

Keep the key and its passwords somewhere safe and backed up, outside the repository. Play App Signing holds the real signing key; this key only uploads.

```bash
keytool -genkeypair -v -keystore ~/keys/hopwyn-upload.jks -alias upload -keyalg RSA -keysize 4096 -validity 10000
```

Then create `android/keystore.properties` (git ignores it). It names the key, not its password:

```
storeFile=/home/you/keys/hopwyn-upload.jks
keyAlias=upload
```

Build the bundle with `scripts/build-android.sh bundle`. It asks for the key's password, or reads it from `HOPWYN_KEY_PASSWORD`, and warns when it falls back to the debug key.

## 2. Play Console

1. **Create app**: name "Hopwyn", app, free.
2. **App content**:
   - **Privacy policy**: the URL above.
   - **Ads**: no ads.
   - **App access**: all features are available without special access. Note for reviewers that a computer running Hopwyn is needed to try pairing.
   - **Content rating**: questionnaire, category Utility. No user-generated content, no sharing of location, no purchases.
   - **Target audience**: 18 and over (not designed for children).
   - **Data safety**: no data collected, no data shared. Data is encrypted in transit: yes. Users can request deletion: not applicable, because nothing is collected. Data stays on the device and the local network.
   - **Accessibility API declaration** (Sensitive permissions → Accessibility): Hopwyn is not an accessibility tool (`isAccessibilityTool` is false). Core feature: "Lets a computer the user paired control this phone with its mouse and keyboard: it shows a pointer, performs taps, swipes, Back and Home, and types into the selected text field." Record a short video: the disclosure dialog, turning the service on, then a computer pointer moving onto the phone and tapping.
3. **Store listing**: copy the title, short and full description from `android/fastlane/metadata/android/en-US/`. Add:
   - the app icon, 512 × 512 PNG;
   - a feature graphic, 1024 × 500;
   - at least two phone screenshots: home with the device map, the trackpad, Settings.
4. **Testing → Internal testing**: upload the `.aab`, add yourself as a tester, install from the Play link, and check pairing, the trackpad and phone control.
5. New personal developer accounts must run a **closed test with at least 12 testers for 14 days** before production access is granted.
6. **Production**: promote the tested release.

## Each release

1. Raise `versionCode`, and `versionName` if it's a user-visible change, in `android/app/build.gradle.kts`.
2. Add `android/fastlane/metadata/android/en-US/changelogs/<versionCode>.txt`.
3. `scripts/build-android.sh bundle`, then upload to internal testing first.
