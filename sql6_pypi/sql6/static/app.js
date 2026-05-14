// sql5 Web Admin - Frontend Logic

const API_BASE = '/api';

// State
let currentTable = null;
let tables = [];

// DOM Elements
const tableList = document.getElementById('table-list');
const sqlEditor = document.getElementById('sql-editor');
const executeBtn = document.getElementById('execute-btn');
const clearBtn = document.getElementById('clear-btn');
const resultsDiv = document.getElementById('results');
const tableStructureDiv = document.getElementById('table-structure');
const tabButtons = document.querySelectorAll('.tab');
const tabContents = document.querySelectorAll('.tab-content');

// Initialize
document.addEventListener('DOMContentLoaded', init);

function init() {
    loadTables();
    setupEventListeners();
}

function setupEventListeners() {
    // Execute button
    executeBtn.addEventListener('click', executeQuery);
    
    // Clear button
    clearBtn.addEventListener('click', () => {
        sqlEditor.value = '';
        resultsDiv.innerHTML = '<div class="placeholder">Execute a query to see results</div>';
    });
    
    // Ctrl+Enter to execute
    sqlEditor.addEventListener('keydown', (e) => {
        if (e.ctrlKey && e.key === 'Enter') {
            e.preventDefault();
            executeQuery();
        }
    });
    
    // Tab switching
    tabButtons.forEach(btn => {
        btn.addEventListener('click', () => {
            const tab = btn.dataset.tab;
            switchTab(tab);
        });
    });
}

// Tab Switching
function switchTab(tabName) {
    tabButtons.forEach(btn => {
        btn.classList.toggle('active', btn.dataset.tab === tabName);
    });
    tabContents.forEach(content => {
        content.classList.toggle('active', content.id === `tab-${tabName}`);
    });
}

// Load Tables
async function loadTables() {
    try {
        const resp = await fetch(`${API_BASE}/tables`);
        const data = await resp.json();
        
        if (data.ok) {
            tables = data.rows.map(r => r[0]);
            renderTableList();
        }
    } catch (e) {
        console.error('Failed to load tables:', e);
    }
}

function renderTableList() {
    if (tables.length === 0) {
        tableList.innerHTML = '<li class="loading">No tables</li>';
        return;
    }
    
    tableList.innerHTML = tables.map(t => `
        <li data-table="${t}">${t}</li>
    `).join('');
    
    // Add click handlers
    tableList.querySelectorAll('li[data-table]').forEach(li => {
        li.addEventListener('click', () => {
            selectTable(li.dataset.table);
        });
    });
}

function selectTable(tableName) {
    currentTable = tableName;
    
    // Update active state
    tableList.querySelectorAll('li').forEach(li => {
        li.classList.toggle('active', li.dataset.table === tableName);
    });
    
    // Show structure
    loadTableStructure(tableName);
    
    // Switch to structure tab
    switchTab('structure');
}

// Load Table Structure
async function loadTableStructure(tableName) {
    try {
        const resp = await fetch(`${API_BASE}/tables/${tableName}`);
        const data = await resp.json();
        
        if (data.ok) {
            renderStructure(data);
            // Also load data
            loadTableData(tableName);
        }
    } catch (e) {
        console.error('Failed to load structure:', e);
    }
}

async function loadTableData(tableName) {
    try {
        const resp = await fetch(`${API_BASE}/execute`, {
            method: 'POST',
            headers: {'Content-Type': 'application/json'},
            body: JSON.stringify({sql: `SELECT * FROM ${tableName}`})
        });
        const data = await resp.json();
        
        if (data.ok) {
            renderTableData(data);
        }
    } catch (e) {
        console.error('Failed to load data:', e);
    }
}

function renderTableData(data) {
    const columns = data.columns || [];
    const rows = data.rows || [];
    
    if (columns.length === 0) {
        return;
    }
    
    let html = '<div class="results" style="margin-top:20px">';
    html += `<div class="results-info">${rows.length} row(s)</div>`;
    html += '<div class="table-container"><table><thead><tr>';
    
    columns.forEach(col => {
        html += `<th>${escapeHtml(col)}</th>`;
    });
    
    html += '</tr></thead><tbody>';
    
    rows.slice(0, 100).forEach(row => {
        html += '<tr>';
        row.forEach(cell => {
            html += `<td>${escapeHtml(String(cell ?? ''))}</td>`;
        });
        html += '</tr>';
    });
    
    html += '</tbody></table></div>';
    
    resultsDiv.innerHTML = html;
}

function renderStructure(data) {
    if (!currentTable) {
        tableStructureDiv.innerHTML = '<div class="placeholder">Select a table</div>';
        return;
    }

    const columns = data.columns || [];
    const rows = data.rows || [];

    if (columns.length === 0 || rows.length === 0) {
        tableStructureDiv.innerHTML = `<div class="placeholder">Table '${currentTable}' is empty</div>`;
        return;
    }

    let html = '<div class="structure-info">';
    html += `<h3>${currentTable}</h3>`;
    html += '<div class="table-columns"><h4>Columns</h4>';

    rows.forEach(row => {
        const name = row[0];
        const type = row[1];
        const nullable = row[2];
        html += `<div class="column-item">`;
        html += `<span class="column-name">${escapeHtml(name)}</span>`;
        html += `<span class="column-type">${escapeHtml(type)}</span>`;
        if (nullable === false) {
            html += `<span class="column-pk">NOT NULL</span>`;
        }
        html += '</div>';
    });

    html += '</div></div>';
    tableStructureDiv.innerHTML = html;
}

// Execute Query
async function executeQuery() {
    const sql = sqlEditor.value.trim();
    if (!sql) {
        resultsDiv.innerHTML = '<div class="error">Please enter a query</div>';
        return;
    }
    
    executeBtn.disabled = true;
    executeBtn.textContent = 'Executing...';
    
    try {
        const resp = await fetch(`${API_BASE}/execute`, {
            method: 'POST',
            headers: {'Content-Type': 'application/json'},
            body: JSON.stringify({sql})
        });
        
        const data = await resp.json();
        
        if (data.ok) {
            renderResults(data);
        } else {
            resultsDiv.innerHTML = `<div class="error">${data.error || 'Query error'}</div>`;
        }
    } catch (e) {
        resultsDiv.innerHTML = `<div class="error">${e.message}</div>`;
    } finally {
        executeBtn.disabled = false;
        executeBtn.textContent = 'Execute (Ctrl+Enter)';
    }
}

function renderResults(data) {
    const columns = data.columns || [];
    const rows = data.rows || [];
    const affected = data.affected || 0;
    
    if (columns.length === 0 && rows.length === 0) {
        resultsDiv.innerHTML = `<div class="results-info">${affected} row(s) affected</div>`;
        return;
    }
    
    let html = `<div class="results-info">${rows.length} row(s)</div>`;
    html += '<div class="table-container"><table><thead><tr>';
    
    // Headers
    columns.forEach(col => {
        html += `<th>${col}</th>`;
    });
    
    html += '</tr></thead><tbody>';
    
    // Rows
    rows.forEach(row => {
        html += '<tr>';
        row.forEach(cell => {
            html += `<td>${escapeHtml(String(cell ?? ''))}</td>`;
        });
        html += '</tr>';
    });
    
    html += '</tbody></table></div>';
    
    resultsDiv.innerHTML = html;
}

function escapeHtml(str) {
    const div = document.createElement('div');
    div.textContent = str;
    return div.innerHTML;
}

// Auto-refresh tables periodically
setInterval(loadTables, 5000);