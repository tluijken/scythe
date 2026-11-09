import { close, equal, test } from "./harness.js";
import {
  barIndexAt, barStep, dbToSlider, faderGain, faderPosition, formatClock, formatDb, formatPan, formatTime, gainToDb, inputLabel,
  meterFraction, sliderToDb,
} from "../js/format.js";

test("fader law matches REAPER and round-trips", () => {
  equal(dbToSlider(0), 716);
  equal(sliderToDb(1000), 12);
  for (const db of [-200, -150, -45, -6, 0, 7.5, 12]) close(sliderToDb(dbToSlider(db)), db, 1e-9, `${db} dB`);
  close(faderPosition(1), 0.716);
  for (const g of [0.01, 0.25, 0.5, 1, 1.9]) close(faderGain(faderPosition(g)) / g, 1, 1e-4, `gain ${g}`);
  equal(faderPosition(0), 0);
  equal(faderGain(0), 0);
  equal(faderPosition(100), 1);
});

test("levels and pan read like the desktop app", () => {
  equal(formatDb(1), "0.0");
  equal(formatDb(0.5), "-6.0");
  equal(formatDb(2), "+6.0");
  equal(formatDb(0), "-inf");
  equal(formatPan(0), "C");
  equal(formatPan(-0.8), "L80");
  equal(formatPan(0.1), "R10");
  equal(gainToDb(0), -150);
});

test("meters map -60..0 dB onto 0..1", () => {
  equal(meterFraction(0), 0);
  equal(meterFraction(1), 1);
  close(meterFraction(0.001), 0);
  close(meterFraction(Math.pow(10, -30 / 20)), 0.5);
  equal(meterFraction(4), 1);
});

test("times and clocks", () => {
  equal(formatTime(0), "0:00.000");
  equal(formatTime(61.5), "1:01.500");
  equal(formatTime(3725.25), "1:02:05.250");
  equal(formatTime(-1), "-0:01.000");
  equal(formatClock(185.9), "3:05");
  equal(formatClock(null), "–:––");
});

test("record inputs in REAPER numbering", () => {
  equal(inputLabel(-1), "None");
  equal(inputLabel(0), "In 1");
  equal(inputLabel(1024 + 2), "In 3/4");
  equal(inputLabel(4096 + 63 * 32), "MIDI all devices, all channels");
});

test("bars are found and spaced", () => {
  const bars = [0, 2, 4, 6];
  equal(barIndexAt(bars, -1), -1);
  equal(barIndexAt(bars, 0), 0);
  equal(barIndexAt(bars, 3.9), 1);
  equal(barIndexAt(bars, 100), 3);
  equal(barStep(60), 1);
  equal(barStep(20), 4);
});
