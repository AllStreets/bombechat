// What this build fits to the machine beyond sound and light, for the pages and the simulator, as edition.css names
// the build. This is the airgapped build's copy, and it fits nothing more. The standard build (bluetooth/) lays its own
// copy of this file over this one, so the two builds share every other page, as they share the app's edition.rs.

// Rows the settings page adds to its own, written as it writes them, one a line. A group with no rows on this build
// shows a blanking plate instead.
export const SETTING_ROWS = [
];

// Every contact is added by checking keys in person (this build has no other way to add one), so New contacts need a
// key check is held On, as the device's edition::ONLY_IN_PERSON has it.
export const ONLY_IN_PERSON = true;

// The simulator's part, called once by createSimulator with what it shares ({ emit, settings, people, test, later,
// bytes, seeded, signer, outbox, now }). It may return start(), setting(id, value, was), channels(), carries(channel)
// with send(m, contactIds, channel), and commands that take the place of the simulator's own. Nothing here.
export function simulate() { return {}; }

// The front page's part, called once the page is up, with the bridge. Nothing here.
export function front() {}
