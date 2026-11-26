# Code signing policy

Lummix is maintained by [lumberjackfrog](https://github.com/lumberjackfrog), the project’s author, reviewer and release approver. Changes from contributors require maintainer review. Windows builds use the public repository and its [installer workflow](../.github/workflows/windows-installer.yml). The maintainer approves each release for distribution.

Lummix is seeking free Windows code signing through the [SignPath Foundation](https://signpath.org/). The current public installer is unsigned. The release notes identify the signing status of each download. Lummix also verifies its own signed release metadata and checks the downloaded installer against the SHA-256 recorded there.

## Privacy

Lummix processes microphone audio, local Spotify playback and voice ducking on the user’s Windows PC. Choosing **Start mix** sends that audio to the selected virtual device. Applications that the user connects to that device, such as VRChat, handle transmission to other people. Settings and diagnostic logs are stored locally. Users can inspect, clear or share their own logs.

First-time setup can contact Spotify and VB-Audio to obtain their installers. Checking for Lummix updates contacts GitHub. The optional wrist controls communicate with XSOverlay on the same PC. These third parties apply their own policies. [Spotify privacy](https://www.spotify.com/legal/privacy-policy/) · [GitHub privacy](https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement) · [VB-Audio](https://vb-audio.com/) · [Third-party notices](../THIRD_PARTY.md)
