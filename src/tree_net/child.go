package tree_net

import (
	"net"
	"fmt"
	"tree_log"
	"tree_node/node_info"
	"tree_lib"
	"strings"
	"tree_api"
	"tree_event"
)

// This file contains functionality for handling parent connections

var (
	parentConnection		*net.TCPConn
	parent_name				string
	listener				*net.TCPListener

	log_from_child		=	"Parent connection handler"
)

func ListenParent() (err tree_lib.TreeError) {
	var (
		addr	*net.TCPAddr
		conn	*net.TCPConn
	)
	err.From = tree_lib.FROM_LISTEN_PARENT
	// If port is not set, setting it to default 8888
	if node_info.CurrentNodeInfo.TreePort == 0 {
		node_info.CurrentNodeInfo.TreePort = 8888
	}

	addr, err.Err = net.ResolveTCPAddr("tcp", fmt.Sprintf("%s:%d", node_info.CurrentNodeInfo.TreeIp, node_info.CurrentNodeInfo.TreePort))
	if !err.IsNull() {
		tree_log.Error(err.From, "Network Listen function", err.Error())
		return
	}

	listener, err.Err = net.ListenTCP("tcp", addr)
	if !err.IsNull() {
		tree_log.Error(err.From, "Network Listen function", err.Error())
		return
	}

	for {
		conn, err.Err = listener.AcceptTCP()
		if !err.IsNull() {
			tree_log.Error(err.From, err.Error())
			return
		}

		// Handle Parent connection
		go handle_api_or_parent_connection(conn)
	}
	return
}

func handle_api_or_parent_connection(conn *net.TCPConn) {
	defer conn.Close()  // Connection should be closed, after return this function
	var (
		err 		tree_lib.TreeError
		msg_data	[]byte
		conn_name	string
		is_api	=	false
	)
	err.From = tree_lib.FROM_HANDLE_API_OR_PARENT_CONNECTION
	// Making basic handshake to check the API validation
	// Connected Parent receiving name of the child(current node) and checking is it valid or not
	// if it is valid name then parent sending his name as an answer
	// otherwise it sending CLOSE_CONNECTION_MARK and closing connection

	_, err.Err = tree_lib.SendMessage([]byte(node_info.CurrentNodeInfo.Name), conn)
	if !err.IsNull() {
		tree_log.Error(err.From, err.Error())
		return
	}

	msg_data, err = tree_lib.ReadMessage(conn)
	if !err.IsNull() {
		tree_log.Error(err.From, err.Error())
		return
	}
	conn_name = string(msg_data)
	if conn_name == CLOSE_CONNECTION_MARK {
		tree_log.Info(err.From, "Connection closed by parent node. Bad tree network handshake ! ", "Parent Addr: ", conn.RemoteAddr().String())
		return
	}

	if strings.Contains(conn_name, tree_api.API_NAME_PREFIX) {
		api_connections[conn_name] = conn
		is_api = true
	} else {
		parent_name = conn_name
		parentConnection = conn
	}

	if is_api {
		tree_event.TriggerWithData(tree_event.ON_API_CONNECTED, []byte(conn_name), nil)
	} else {
		tree_event.TriggerWithData(tree_event.ON_PARENT_CONNECTED, []byte(conn_name), nil)
	}

	// Listening parent messages
	for {
		msg_data, err = tree_lib.ReadMessage(conn)
		if !err.IsNull() {
			tree_log.Error(err.From, " reading data from -> ", conn_name, " ", err.Error())
			break
		}

		// Handling message events
		handle_message(is_api, true, msg_data)
	}

	if is_api {
		api_connections[conn_name] = nil
		delete(api_connections, conn_name)
		tree_event.TriggerWithData(tree_event.ON_API_DISCONNECTED, []byte(conn_name), nil)
	} else {
		parentConnection = nil
		tree_event.TriggerWithData(tree_event.ON_PARENT_DISCONNECTED, []byte(conn_name), nil)
	}
}