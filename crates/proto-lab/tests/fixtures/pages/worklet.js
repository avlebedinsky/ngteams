// An audio worklet processor that writes a constant: it runs only once its target is resumed.
class LabTone extends AudioWorkletProcessor {
  process(_inputs, outputs) {
    for (const channel of outputs[0]) {
      channel.fill(0.25);
    }
    return true;
  }
}
registerProcessor('lab-tone', LabTone);
