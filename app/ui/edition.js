// What this build fits to the machine beyond sound and light, for the pages and the simulator, as edition.css names
// the build. This is the airgapped build's copy, and it fits nothing more. The standard build (bluetooth/) lays its own
// copy of this file over this one, so the two builds share every other page, as they share the app's edition.rs.

// Rows the settings page adds to its own, written as it writes them, one a line. A group with no rows on this build
// shows a blanking plate instead.
export const SETTING_ROWS = [
];

// What the wiring schedule types under a group's name. Under a group this build has no rows for (its blanking plate),
// why it has none.
export const GROUP_NOTES = {
  Bluetooth: 'This build carries no Bluetooth code, so there is nothing here to switch on. The standard build adds discovery and requests to connect from people you have not met.',
};

// Every contact is added by checking keys in person (this build has no other way to add one), so New contacts need a
// key check is held On, as the device's edition::ONLY_IN_PERSON has it.
export const ONLY_IN_PERSON = true;

// The simulator's part, called once by createSimulator with what it shares ({ emit, settings, people, test, later,
// bytes, seeded, signer, outbox, now }). It may return start(), setting(id, value, was), channels(), carries(channel)
// with send(m, contactIds, channel), and commands that take the place of the simulator's own. Nothing here.
export function simulate() { return {}; }

// The front page's part, called once the page is up, before the bridge starts, with the bridge and what the page lends
// it (bombechat.html: a switch fitted as Sound and Light are, the top bar, the stations, the key check card). Nothing
// here.
export function front() {}
