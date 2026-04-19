const mqtt = require('mqtt');

const client = mqtt.connect('mqtt://127.0.0.1:1883');

client.on('connect', () => {
    client.subscribe('neurostrata/response', () => {
        client.publish('neurostrata/request', JSON.stringify({
            request_id: "test",
            action: "generate_canvas",
            payload: { namespace: "TestNamespace" }
        }));
    });
});

client.on('message', (topic, msg) => {
    console.log("Response:", msg.toString());
    process.exit(0);
});