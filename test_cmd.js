const fs = require('fs');
const plasticity = JSON.parse(fs.readFileSync('plasticity.json', 'utf8'));
const setup_script = plasticity.sandbox.setup_script.join(" && ");
const agent_command = plasticity.agent_command;
const quoted_agent_cmd = agent_command.map(s => {
  if (s.includes(" ") || s.includes("\"") || s.includes("'") || s.includes("*") || s.includes("$")) {
    return `'${s.replace(/'/g, "'\\''")}'`;
  }
  return s;
});
const full_command = `${setup_script} && ${quoted_agent_cmd.join(" ")}`;
console.log(full_command);
