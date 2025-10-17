const sessions = new Map();

export default {
  async fetch(request, env) {

    const upgradeHeader = request.headers.get('Upgrade');
    if (!upgradeHeader || upgradeHeader !== 'websocket') {
      return new Response('Expected a WebSocket Upgrade request', { status: 426 });
    }
    const webSocketPair = new WebSocketPair();
    const [client, server] = Object.values(webSocketPair);

    server.accept();
    server.addEventListener('message', event => {
      try {
        const message = JSON.parse(event.data);
        console.log("Received message:", message);
        if (message.type === 'register' && message.code && message.localIp) {
          console.log(`Registering session '${message.code}' for IP ${message.localIp}`);
          sessions.set(message.code, message.localIp);
          server.send(JSON.stringify({ type: 'registered', status: 'success' }));
        }
        else if (message.type === 'request' && message.code) {
          console.log(`Received request for session '${message.code}'`);
          const localIp = sessions.get(message.code);
        if (localIp) {
            console.log(`Found IP ${localIp}, sending to client.`);
            server.send(JSON.stringify({ type: 'found', localIp: localIp }));
            sessions.delete(message.code);
          } else {
            console.log(`Session '${message.code}' not found.`);
            server.send(JSON.stringify({ type: 'not_found' }));
          }
        }
        else {
          server.send(JSON.stringify({ type: 'error', message: 'Invalid message format' }));
        }
      } catch (error) {
        server.send(JSON.stringify({ type: 'error', message: 'Failed to parse JSON message' }));
        console.error("Failed to handle message:", error);
      }
    });
    server.addEventListener('close', () => {
      console.log('A client disconnected.');
    });
    return new Response(null, {
      status: 101, 
      webSocket: client,
    });
  },
};


