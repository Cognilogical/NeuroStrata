with open('src/bin/migrate.rs', 'r') as f:
    content = f.read()

content = content.replace('let namespace = point["payload"]["user_id"].as_str().unwrap_or("global").to_string();', 'let mut namespace = point["payload"]["user_id"].as_str().unwrap_or("global").to_string();\n        namespace = namespace.replace("/", "_");\n        namespace = namespace.replace(" ", "_");\n        if namespace.starts_with("_") { namespace = namespace[1..].to_string(); }')

with open('src/bin/migrate.rs', 'w') as f:
    f.write(content)
