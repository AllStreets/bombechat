# Bombechat

Bombechat is a messenger that never touches the internet. Messages travel between nearby devices by sound, by light, or by Bluetooth, and can hop from device to device to reach people further away. It is meant for when the network is down, and for when you would rather it not be involved at all.

It is at the design stage. This repo holds the interface mockups and the checks I run on them. There is no messaging code yet.

![The Bombechat mockup: a bank of 36 code drums above a message sheet, a key check card, and the channel switches](mockups/bombechat.png)

## The idea

Most phones and laptops already have what this needs: a speaker and a microphone, a screen and a camera, and usually a Bluetooth radio.

- Sound carries short messages across a room on tones between 17.6 and 19.8 kHz, which most adults can barely hear.
- Light carries them between screens and cameras in line of sight, which makes it the hardest channel to overhear.
- Bluetooth has the longest reach and works on nearly every device.

There is a separate build for airgapped computers. It contains no Bluetooth code at all, so there is nothing to switch off and nothing to trust. Most people will have met in person and swapped keys by flashing a code at each other's camera. On the standard build, a stranger in Bluetooth range can send a request to connect, and nothing is exchanged unless you accept it. Contacts are stored encrypted on the device.

## The interface

The look is taken from the Bombe, the machine Alan Turing designed at Bletchley Park to find each day's Enigma settings. The window is the machine and its paperwork. Three rows of drums show the key, what the microphone heard, and the plain text. When a frame of a message arrives, the drums turn and the top row settles on the words, which are then pasted onto the message sheet as teleprinter tape. Your own replies are typed straight onto the sheet. The key check is a pencil menu on squared paper, the way Bombe menus were drawn.

The drums make a ratchet click for every letter that passes the index and a clunk when they settle. The sound is synthesised in the browser and filtered hard above 7 kHz, so it stays out of in the band Bombechat listens on. Click anywhere to allow sound, and use the switch in the title bar to turn it off.

Open `mockups/bombechat.html` in a browser to see it move. The earlier directions, including a spectrogram-based "Cyber" design, are in `mockups/explorations`.

## Checks

Every change to the interface goes through the same check before it counts as done:

```
npm install
npm run check
```

`tools/check.cjs` loads the page in Chromium and saves a screenshot of every message frame, close-up crops of each part of the window at double resolution, and a capture of the drums mid-turn. It confirms that the plain-text row settles on the right letters, then renders one drum turn offline and measures it: how many distinct clicks it makes, whether it clips, and how much of its energy falls in the listening band. It fails on any page error. The output goes to `checks/`, which is not committed.

The fonts on the engraved plates use Copperplate, which ships with macOS. A web release will need a licensed engraving face.
