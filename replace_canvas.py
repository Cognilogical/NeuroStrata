import re
import sys

def replace_in_file(filepath):
    with open(filepath, 'r') as f:
        content = f.read()

    replacement = """
                        let mut nodes = Vec::new();
                        let mut edges = Vec::new();

                        let doc_width = 400;
                        let doc_height = 400;
                        let mem_width = 300;
                        let mem_height = 150;
                        let x_gap = 500;
                        let y_gap = 50;

                        let mut domain_map: std::collections::HashMap<String, Vec<&crate::store::lancedb::SearchResult>> = std::collections::HashMap::new();
                        let mut orphaned_memories = Vec::new();

                        for m in &all_memories {
                            let mut found_doc = false;
                            
                            // Check refs first
                            if let Some(refs) = m.payload.metadata.get("refs").and_then(|r| r.as_array()) {
                                for r in refs {
                                    if let Some(file) = r.get("file").and_then(|f| f.as_str()) {
                                        domain_map.entry(file.to_string()).or_default().push(m);
                                        found_doc = true;
                                    }
                                }
                            }
                            
                            // Check location if no refs found
                            if !found_doc && !m.payload.location.is_empty() {
                                domain_map.entry(m.payload.location.clone()).or_default().push(m);
                                found_doc = true;
                            }

                            if !found_doc {
                                orphaned_memories.push(m);
                            }
                        }

                        let start_x = -1000;
                        let mut start_y = -1000;
                        let mut max_structured_y = start_y + doc_height;

                        // Create sets for fast lookup of related_to
                        let mut existing_ids = std::collections::HashSet::new();
                        for m in &all_memories {
                            existing_ids.insert(m.id.clone());
                        }

                        // Build File Nodes & Grouped Memories
                        let mut col_index = 0;
                        for (doc_path, memories) in &domain_map {
                            let x = start_x + (col_index * (doc_width + x_gap));
                            let y = start_y;
                            let doc_node_id = format!("doc-{}", col_index);

                            nodes.push(serde_json::json!({
                                "id": doc_node_id,
                                "type": "file",
                                "file": doc_path,
                                "x": x,
                                "y": y,
                                "width": doc_width,
                                "height": doc_height,
                                "color": "4"
                            }));

                            for (row_index, m) in memories.iter().enumerate() {
                                let mem_node_id = format!("mem-{}", m.id);
                                let mem_x = x + doc_width + 100;
                                let mem_y = y + (row_index as i32 * (mem_height + y_gap));

                                let domain = m.payload.metadata.get("domain").and_then(|d| d.as_str()).unwrap_or("general");
                                let r_type = &m.payload.memory_type;
                                let text = format!("### {}\\n**Domain:** {}\\n\\n{}", r_type.to_uppercase(), domain, m.payload.content);

                                nodes.push(serde_json::json!({
                                    "id": mem_node_id,
                                    "type": "text",
                                    "text": text,
                                    "x": mem_x,
                                    "y": mem_y,
                                    "width": mem_width,
                                    "height": mem_height,
                                    "color": "3"
                                }));

                                edges.push(serde_json::json!({
                                    "id": format!("edge-doc-{}", m.id),
                                    "fromNode": mem_node_id,
                                    "fromSide": "left",
                                    "toNode": doc_node_id,
                                    "toSide": "right",
                                    "color": "5"
                                }));
                                
                                // Related_to edges
                                if let Some(related) = m.payload.metadata.get("related_to").and_then(|r| r.as_array()) {
                                    for target_val in related {
                                        if let Some(target_id) = target_val.as_str() {
                                            if existing_ids.contains(target_id) {
                                                edges.push(serde_json::json!({
                                                    "id": format!("edge-rel-{}-{}", m.id, target_id),
                                                    "fromNode": mem_node_id,
                                                    "fromSide": "right",
                                                    "toNode": format!("mem-{}", target_id),
                                                    "toSide": "left",
                                                    "color": "4"
                                                }));
                                            }
                                        }
                                    }
                                }
                            }

                            let column_max_y = y + (memories.len() as i32 * (mem_height + y_gap));
                            if column_max_y > max_structured_y {
                                max_structured_y = column_max_y;
                            }
                            col_index += 1;
                        }

                        // Orphaned Memories
                        if !orphaned_memories.is_empty() {
                            let orph_start_y = max_structured_y + 400;
                            let cols = 4;
                            let rows = (orphaned_memories.len() as f32 / cols as f32).ceil() as i32;
                            
                            let orph_group_width = (cols * mem_width) + ((cols - 1) * 50) + 100;
                            let orph_group_height = (rows * mem_height) + ((rows - 1) * 50) + 100;

                            nodes.push(serde_json::json!({
                                "id": "group-orphans",
                                "type": "group",
                                "x": start_x - 50,
                                "y": orph_start_y - 50,
                                "width": orph_group_width,
                                "height": orph_group_height,
                                "label": "Orphaned Memories (No document links)",
                                "color": "1"
                            }));

                            for (i, m) in orphaned_memories.iter().enumerate() {
                                let col = (i as i32) % cols;
                                let row = (i as i32) / cols;
                                
                                let domain = m.payload.metadata.get("domain").and_then(|d| d.as_str()).unwrap_or("general");
                                let r_type = &m.payload.memory_type;
                                let text = format!("### {}\\n**Domain:** {}\\n\\n{}", r_type.to_uppercase(), domain, m.payload.content);
                                let mem_node_id = format!("mem-{}", m.id);

                                nodes.push(serde_json::json!({
                                    "id": mem_node_id,
                                    "type": "text",
                                    "text": text,
                                    "x": start_x + (col * (mem_width + 50)),
                                    "y": orph_start_y + (row * (mem_height + 50)),
                                    "width": mem_width,
                                    "height": mem_height,
                                    "color": "1"
                                }));
                                
                                // Related_to edges
                                if let Some(related) = m.payload.metadata.get("related_to").and_then(|r| r.as_array()) {
                                    for target_val in related {
                                        if let Some(target_id) = target_val.as_str() {
                                            if existing_ids.contains(target_id) {
                                                edges.push(serde_json::json!({
                                                    "id": format!("edge-rel-{}-{}", m.id, target_id),
                                                    "fromNode": mem_node_id,
                                                    "fromSide": "right",
                                                    "toNode": format!("mem-{}", target_id),
                                                    "toSide": "left",
                                                    "color": "4"
                                                }));
                                            }
                                        }
                                    }
                                }
                            }
                        }
"""

    pattern = r'let mut nodes = Vec::new\(\);.*?if let Some\(related\) = m\s*\.payload\s*\.metadata\s*\.get\("related_to"\).*?\}\s*\}\s*\}\s*\}'
    
    new_content = re.sub(pattern, replacement.strip(), content, flags=re.DOTALL | re.MULTILINE)
    
    with open(filepath, 'w') as f:
        f.write(new_content)

replace_in_file("src/mqtt_worker.rs")
replace_in_file("src/server.rs")
