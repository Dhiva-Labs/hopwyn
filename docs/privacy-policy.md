# Hopwyn privacy policy

_Last updated: 21 November 2026_

Hopwyn lets the computers and phones in your home share one mouse and keyboard. This policy covers the Hopwyn apps for Android, Linux, Windows and macOS, published by Dhiva Labs.

## The short version

Hopwyn collects no personal data. It has no account, no cloud service, no analytics, no advertising and no crash reporting. Nothing you do in Hopwyn is sent to Dhiva Labs or to anyone else.

## What stays on your devices

- **Device keys and pairings.** Each device creates its own encryption key on first launch. When you pair two devices, each stores the other's public key and name so they can recognise each other later. These stay on your devices; Hopwyn never uploads them.
- **Settings.** Your choices, such as theme and pointer speed, are saved on the device.
- **Device name.** Hopwyn announces your device's name (for example "Pixel 9") on your local network so your other devices can list it. It is visible only to devices on the same network.

Uninstalling Hopwyn, or clearing its storage, deletes all of the above.

## What goes over your network

Paired devices talk to each other directly over your home network. They send pointer movements, clicks, scrolling, key presses and typed text, only while you are using one device to control another. This traffic is end-to-end encrypted and authenticated with the keys you checked when pairing. It never passes through a Dhiva Labs server, because there isn't one.

## Accessibility service (Android)

If you turn it on, Hopwyn uses Android's Accessibility service so that a computer you paired can control your phone. With it on, Hopwyn:

- shows a pointer and performs taps and swipes for the computer's clicks, drags and scrolling;
- presses Back and Home;
- types into the text field you have selected, which needs Hopwyn to find that field on screen.

Hopwyn does not read, collect, store or send what is on your screen, and it acts only while a paired computer is connected and controlling the phone. You can turn the service off at any time in **Settings → Accessibility → Hopwyn**.

## Permissions

| Permission | Why |
| --- | --- |
| Network access, Wi-Fi state, multicast | Find your other devices on the local network and connect to them. |
| Accessibility service (optional) | Let a paired computer control the phone, as described above. |

## Children

Hopwyn is not directed at children and collects no data from anyone.

## Changes

If this policy changes, the new version will be published at this address with a new date above.

## Contact

Questions about privacy: open an issue at <https://github.com/Dhiva-Labs/hopwyn/issues>.
