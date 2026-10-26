"""gen-dispatch.py over a few frames of the SDK's command-functions.h, with
a stand-in arm-none-eabi-nm naming the library callbacks: no SDK needed.

Run from the top: python3 -m unittest discover -s platform/common/radio/tests
"""
import os
import subprocess
import sys
import tempfile
import unittest

GEN = os.path.join(os.path.dirname(__file__), "..", "gen-dispatch.py")

# As app/ncp/plugin/plugin.info has them: ezsp-zll is not one gen-dispatch.py builds in.
PLUGIN_INFO = """\
plugin(ezsp-zigbee-pro) {
  name=Zigbee PRO Library EZSP Command Handlers

  setup(ezsp) {
    sections = Networking Frames, Messaging Frames

    # Networking Frames
    networkInit                 = emberNetworkInit
    sendUnicast                 = emberAfPluginEzspZigbeeProSendUnicastCommandCallback
  }
}

plugin(ezsp-security) {
  setup(ezsp) {
    sections = Security Frames
  }
}

plugin(ezsp-binding) {
  setup(ezsp) {
    sections = Binding Frames
  }
}

plugin(ezsp-zll) {
  setup(ezsp) {
    sections = ZLL Frames
    zllNetworkOps = emberZllNetworkOps
  }
}
"""

# Verbatim from the SDK, but for ezspZllNetworkOps, cut down.
COMMAND_FUNCTIONS = """\
uint8_t ezspVersion(
  uint8_t desiredProtocolVersion,
  uint8_t *stackType,
  uint16_t *stackVersion)
{
  uint8_t protocolVersion;
  startCommand(EZSP_VERSION);
  appendInt8u(desiredProtocolVersion);
  EzspStatus sendStatus = sendCommand();
  if (sendStatus == EZSP_SUCCESS) {
    protocolVersion = fetchInt8u();
    *stackType = fetchInt8u();
    *stackVersion = fetchInt16u();
    return protocolVersion;
  }
  return 255;
}

uint8_t ezspEcho(
  uint8_t dataLength,
  uint8_t *data,
  uint8_t *echo)
{
  uint8_t echoLength;
  startCommand(EZSP_ECHO);
  appendInt8u(dataLength);
  appendInt8uArray(dataLength, data);
  EzspStatus sendStatus = sendCommand();
  if (sendStatus == EZSP_SUCCESS) {
    echoLength = fetchInt8u();
    fetchInt8uArray(echoLength, echo);
    return echoLength;
  }
  return 0;
}

void ezspGetEui64(
  EmberEUI64 eui64)
{
  startCommand(EZSP_GET_EUI64);
  EzspStatus sendStatus = sendCommand();
  if (sendStatus == EZSP_SUCCESS) {
    fetchInt8uArray(8, eui64);
  }
}

EmberStatus ezspNetworkInit(
  EmberNetworkInitStruct *networkInitStruct)
{
  uint8_t status;
  startCommand(EZSP_NETWORK_INIT);
  appendEmberNetworkInitStruct(networkInitStruct);
  EzspStatus sendStatus = sendCommand();
  if (sendStatus == EZSP_SUCCESS) {
    status = fetchInt8u();
    return status;
  }
  return sendStatus;
}

EmberStatus ezspGetNetworkParameters(
  EmberNodeType *nodeType,
  EmberNetworkParameters *parameters)
{
  uint8_t status;
  startCommand(EZSP_GET_NETWORK_PARAMETERS);
  EzspStatus sendStatus = sendCommand();
  if (sendStatus == EZSP_SUCCESS) {
    status = fetchInt8u();
    *nodeType = fetchInt8u();
    fetchEmberNetworkParameters(parameters);
    return status;
  }
  return sendStatus;
}

EmberStatus ezspSendUnicast(
  EmberOutgoingMessageType type,
  EmberNodeId indexOrDestination,
  EmberApsFrame *apsFrame,
  uint8_t messageTag,
  uint8_t messageLength,
  uint8_t *messageContents,
  uint8_t *sequence)
{
  uint8_t status;
  startCommand(EZSP_SEND_UNICAST);
  appendInt8u(type);
  appendInt16u(indexOrDestination);
  appendEmberApsFrame(apsFrame);
  appendInt8u(messageTag);
  appendInt8u(messageLength);
  appendInt8uArray(messageLength, messageContents);
  EzspStatus sendStatus = sendCommand();
  if (sendStatus == EZSP_SUCCESS) {
    status = fetchInt8u();
    *sequence = fetchInt8u();
    return status;
  }
  return sendStatus;
}

EmberStatus ezspZllNetworkOps(
  uint8_t op)
{
  uint8_t status;
  startCommand(EZSP_ZLL_NETWORK_OPS);
  appendInt8u(op);
  EzspStatus sendStatus = sendCommand();
  if (sendStatus == EZSP_SUCCESS) {
    status = fetchInt8u();
    return status;
  }
  return sendStatus;
}
"""

# The callbacks the libraries define, and one they only use.
NM = """\

command-handlers.o:
00000001 T emberAfEzspGetEui64CommandCallback
00000041 T emberAfEzspGetNetworkParametersCommandCallback
00000081 T emberAfEzspEchoCommandCallback
         U emberAfEzspVersionCommandCallback
"""


class GenDispatch(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.dir = tempfile.TemporaryDirectory()
        root = cls.dir.name
        zb = os.path.join(root, "zigbee")
        for path, text in [("app/ncp/plugin/plugin.info", PLUGIN_INFO),
                           ("app/util/ezsp/command-functions.h", COMMAND_FUNCTIONS)]:
            os.makedirs(os.path.dirname(os.path.join(zb, path)), exist_ok=True)
            with open(os.path.join(zb, path), "w") as f:
                f.write(text)
        bin_dir = os.path.join(root, "bin")
        os.makedirs(bin_dir)
        with open(os.path.join(root, "nm.out"), "w") as f:
            f.write(NM)
        nm = os.path.join(bin_dir, "arm-none-eabi-nm")
        with open(nm, "w") as f:
            f.write('#!/bin/sh\ncat "%s"\n' % os.path.join(root, "nm.out"))
        os.chmod(nm, 0o755)
        env = dict(os.environ, PATH=bin_dir + os.pathsep + os.environ.get("PATH", ""))
        run = subprocess.run([sys.executable, GEN, zb, "lib.a"], capture_output=True, text=True, env=env)
        if run.returncode != 0:
            raise AssertionError(run.stderr)
        cls.out, cls.err = run.stdout, run.stderr

    @classmethod
    def tearDownClass(cls):
        cls.dir.cleanup()

    def case(self, command):
        """The lines of `command`'s case, without its return."""
        lines = self.out.splitlines()
        start = lines.index(f"    case {command}: {{")
        end = lines.index("      return true;", start)
        return [line.strip() for line in lines[start + 1:end]]

    def test_a_plugin_mapped_frame_mirrors_the_host_function(self):
        self.assertEqual(self.case("EZSP_SEND_UNICAST"), [
            "EmberOutgoingMessageType type = (EmberOutgoingMessageType)fetchInt8u();",
            "EmberNodeId indexOrDestination = (EmberNodeId)fetchInt16u();",
            "EmberApsFrame apsFrame;",
            "fetchEmberApsFrame(&apsFrame);",
            "uint8_t messageTag = (uint8_t)fetchInt8u();",
            "uint8_t messageLength = (uint8_t)fetchInt8u();",
            "uint8_t *messageContents = fetchInt8uPointer(messageLength);",
            "uint8_t sequence;",
            "EmberStatus status = emberAfPluginEzspZigbeeProSendUnicastCommandCallback(type, indexOrDestination, "
            "&apsFrame, messageTag, messageLength, messageContents, &sequence);",
            "appendInt8u(status);",
            "appendInt8u(sequence);",
        ])
        self.assertEqual(self.case("EZSP_NETWORK_INIT"), [
            "EmberNetworkInitStruct networkInitStruct;",
            "fetchEmberNetworkInitStruct(&networkInitStruct);",
            "EmberStatus status = emberNetworkInit(&networkInitStruct);",
            "appendInt8u(status);",
        ])

    def test_a_library_callback_answers_its_frame(self):
        self.assertEqual(self.case("EZSP_GET_EUI64"), [
            "uint8_t eui64[8];",
            "emberAfEzspGetEui64CommandCallback(eui64);",
            "appendInt8uArray(8, eui64);",
        ])
        self.assertEqual(self.case("EZSP_GET_NETWORK_PARAMETERS"), [
            "EmberNodeType nodeType;",
            "EmberNetworkParameters parameters;",
            "EmberStatus status = emberAfEzspGetNetworkParametersCommandCallback(&nodeType, &parameters);",
            "appendInt8u(status);",
            "appendInt8u(nodeType);",
            "appendEmberNetworkParameters(&parameters);",
        ])

    def test_handlers_without_a_header_are_declared(self):
        protos = " ".join(self.out.split("bool emAfProcessEzspCommand")[0].split())
        self.assertIn("EmberStatus emberAfPluginEzspZigbeeProSendUnicastCommandCallback(EmberOutgoingMessageType type,", protos)
        self.assertIn("void emberAfEzspGetEui64CommandCallback(EmberEUI64 eui64);", protos)
        # The stack's own functions have their headers.
        self.assertNotIn("emberNetworkInit(", protos)

    def test_frames_without_a_handler_are_left_to_the_core(self):
        # version: the core's, with only an undefined reference in the libraries; ZLL: not built in.
        for command in ["EZSP_VERSION", "EZSP_ZLL_NETWORK_OPS"]:
            self.assertNotIn(f"case {command}:", self.out)

    def test_a_frame_with_no_mirror_is_left_out_and_named(self):
        # echo's answer is an array of a length read from the answer itself.
        self.assertNotIn("case EZSP_ECHO:", self.out)
        self.assertIn("gen-dispatch: no mirror for echo", self.err)

    def test_the_dispatcher_falls_through_to_the_core(self):
        self.assertIn("bool emAfProcessEzspCommand(uint16_t commandId)", self.out)
        self.assertTrue(self.out.rstrip().endswith("default:\n      return false;\n  }\n}"))


if __name__ == "__main__":
    unittest.main()
