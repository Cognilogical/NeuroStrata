// NeuroStrata TUI entry for OpenCode 2.x. Display only: the server plugin
// (plugins/core, OPENCODE_USER_LINE_RPC) decides every text and emits it as
// the RPC event `user_line`; this file shows each one as a toast. Installed
// as `tui.js` next to the server entry in .opencode/plugins/neurostrata/.

export const NEUROSTRATA_USER_LINE_RPC = {
  id: "neurostrata",
  methods: {},
  events: {
    user_line: {
      schema: {
        type: "object",
        properties: {
          message: { type: "string" },
          variant: { type: "string", enum: ["info", "warning", "error"] },
        },
        required: ["message", "variant"],
        additionalProperties: false,
      },
    },
  },
};

export default {
  id: "neurostrata-tui",
  setup(context) {
    return context.client.rpc(NEUROSTRATA_USER_LINE_RPC).events.on("user_line", (event) => {
      context.ui.toast.show({ title: "NeuroStrata", message: event.data.message, variant: event.data.variant });
    });
  },
};
